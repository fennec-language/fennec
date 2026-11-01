//! Generators that are passed around, as state machines. A yield function's body is lowered as
//! usual, then each `yield` becomes a place to stop: what's needed afterward goes into the
//! generator's block, and resuming puts it back and carries on from there.
//!
//! So a yield function `g` makes two functions. `g` makes the block. `g.resume` takes the block
//! and gives back whether there's another item, which is in the block's item slot.

use std::collections::{BTreeSet, HashMap};

use fennec_check::Type;
use fennec_syntax::{BinOp, Expr, ExprKind, FnDef, Span, Stmt, StmtKind};

use super::generics::FunctionInstance;
use super::lists::Over;
use super::{Context, Loop, Lowered, Lowering, Tail};
use crate::costs::{inst_reads, terminator_reads};
use crate::{Function, GeneratorDef, Inst, InstKind, Local, LocalId, Operand, Terminator, slot};

/// A `yield` in a body being lowered: the block it ends, what it gives, and what comes next.
pub(super) struct Yield {
    block: crate::BlockId,
    value: Operand,
    next: crate::BlockId,
}

/// What a yield function being lowered knows.
pub(super) struct Generating {
    item: Type,
    yields: Vec<Yield>,
}

/// What a generator's `pc` holds when it isn't where to go on from.
const FINISHED: i64 = -1;
const RUNNING: i64 = -2;
const FAILED: i64 = -3;

/// A generator's type's item, and the error it can fail with, from what its call gives back.
fn parts(returns: &Type) -> Lowered<(Type, Option<Type>)> {
    match returns {
        Type::Iterator(item) => Ok(((**item).clone(), None)),
        Type::Result(ok, error) => match &**ok {
            Type::Iterator(item) => Ok(((**item).clone(), Some((**error).clone()))),
            other => Err(format!("it's a generator giving back {other}")),
        },
        other => Err(format!("it's a generator giving back {other}")),
    }
}

/// The name of the function that resumes a generator.
pub(super) fn resume_name(generator: &str) -> String {
    format!("{generator}.resume")
}

impl<'a> Lowering<'a> {
    /// A yield function, as the function that makes its block and the one that resumes it.
    pub(super) fn generator(
        instance: &FunctionInstance,
        def: &FnDef,
        context: &'a Context<'a>,
        captures: &[(String, Type)],
        signature: &fennec_check::Signature,
    ) -> Lowered<Vec<Function>> {
        let (item, error) = parts(&signature.returns)?;
        let body = context.signatures.types().body(&instance.base).ok_or("it has no types")?;
        let returns = error.map_or(Type::Unit, |error| Type::Result(Box::new(Type::Unit), Box::new(error)));
        let mut lowering = Lowering::new(instance, context, body, returns);
        lowering.begin_generator(item);
        let mut params = Vec::new();
        for (name, ty) in captures {
            params.push(lowering.declare(name, ty.clone()));
        }
        for param in &signature.params {
            if param.is_mut {
                return Err("a generator has a `mut` parameter".to_owned());
            }
            params.push(lowering.declare(&param.name, param.ty.clone()));
        }
        lowering.stmts(&def.body, Tail::Drop)?;
        lowering.end_function(def.span)?;
        lowering.finish_generator(&params, &signature.returns, def.span)
    }

    /// Starts lowering a generator: its block is the first local, and the first block is left
    /// for choosing where to go on from.
    pub(super) fn begin_generator(&mut self, item: Type) {
        self.declare("\0self", Type::Iterator(Box::new(item.clone())));
        self.generating = Some(Generating { item, yields: Vec::new() });
        let body = self.new_block();
        self.finish(Terminator::Jump(body));
        self.current = body;
    }

    /// `yield value`: stops here, to go on from the next block.
    pub(super) fn yield_value(&mut self, value: &Expr, span: Span) -> Lowered<()> {
        let item = self.generating.as_ref().ok_or("it has `yield` outside a generator")?.item.clone();
        let lowered = self.expr(value)?;
        let lowered = self.coerce(lowered, &item, span)?;
        self.yield_operand(lowered);
        Ok(())
    }

    fn yield_operand(&mut self, value: Operand) {
        let block = self.current;
        let next = self.new_block();
        self.finish(Terminator::Jump(next));
        self.current = next;
        if let Some(generating) = &mut self.generating {
            generating.yields.push(Yield { block, value, next });
        }
    }

    /// Turns the finished body into the function that resumes it, and adds the one that makes
    /// the block. `params` are what the block starts with, and `returns` is what the call gives.
    pub(super) fn finish_generator(
        mut self,
        params: &[LocalId],
        returns: &Type,
        span: Span,
    ) -> Lowered<Vec<Function>> {
        let generating = self.generating.take().ok_or("a generator with nothing yielded")?;
        let name = self.name.clone();
        let live = self.live_in();
        let live_at = |block: crate::BlockId| live.get(block.0).cloned().unwrap_or_default();
        let body = crate::BlockId(1);
        // The first local is the block itself, which is always there.
        let mut saved: BTreeSet<LocalId> = live_at(body);
        for stop in &generating.yields {
            saved.extend(live_at(stop.next));
        }
        saved.remove(&LocalId(0));
        let slots: HashMap<LocalId, usize> =
            saved.iter().enumerate().map(|(at, local)| (*local, slot::LOCALS + at)).collect();
        let types: Vec<Type> = saved.iter().map(|local| self.local_type(*local)).collect();
        let saved_at = |block: crate::BlockId| -> Vec<(LocalId, usize)> {
            live_at(block)
                .into_iter()
                .filter_map(|local| slots.get(&local).map(|slot| (local, *slot)))
                .collect()
        };
        let mut places = vec![(0, body, saved_at(body))];
        for (at, stop) in generating.yields.iter().enumerate() {
            let pc = i64::try_from(at).map_err(|_| "too many yields")? + 1;
            self.stop_here(stop, pc, &saved_at(stop.next), span);
            places.push((pc, stop.next, saved_at(stop.next)));
        }
        self.finish_returns(span);
        self.choose_where_to_go_on(&places, span);
        let fails = matches!(returns, Type::Result(..));
        self.returns = Type::Bool;
        self.counts.clear();
        let context = self.context;
        let mut functions = self.finished(1, Vec::new());
        if let Some(resume) = functions.first_mut() {
            resume.name = resume_name(&name);
        }
        let maker = make_block_function(&name, params, &slots, (returns, &types), &functions, span);
        context.generator_defs.borrow_mut().push(GeneratorDef {
            name: name.clone(),
            item: generating.item,
            slots: types,
            resume: resume_name(&name),
            abandon: None,
            fails,
        });
        functions.insert(0, maker);
        Ok(functions)
    }

    /// Which locals each block needs from before it.
    fn live_in(&self) -> Vec<BTreeSet<LocalId>> {
        let yielded: HashMap<usize, &Operand> = self
            .generating
            .iter()
            .flat_map(|generating| &generating.yields)
            .map(|stop| (stop.block.0, &stop.value))
            .collect();
        let mut live: Vec<BTreeSet<LocalId>> = vec![BTreeSet::new(); self.blocks.len()];
        loop {
            let mut changed = false;
            for (at, block) in self.blocks.iter().enumerate().rev() {
                let mut here: BTreeSet<LocalId> = BTreeSet::new();
                if let Some(end) = &block.end {
                    for target in end.targets() {
                        here.extend(live.get(target.0).into_iter().flatten().copied());
                    }
                    here.extend(terminator_reads(end));
                }
                if let Some(Operand::Local(local)) = yielded.get(&at) {
                    here.insert(*local);
                }
                for inst in block.insts.iter().rev() {
                    if let Some(dest) = inst.kind.dest() {
                        here.remove(&dest);
                    }
                    here.extend(inst_reads(&inst.kind));
                }
                if live.get(at) != Some(&here)
                    && let Some(slot) = live.get_mut(at)
                {
                    *slot = here;
                    changed = true;
                }
            }
            if !changed {
                return live;
            }
        }
    }

    fn set_slot(&mut self, at: usize, value: Operand, span: Span) {
        let generator = Operand::Local(LocalId(0));
        self.emit(InstKind::SetSlot { generator, slot: at, value }, span);
    }

    /// Ends a block at a `yield`: keeps the item and what's needed later, and says there's one.
    fn stop_here(&mut self, stop: &Yield, pc: i64, saved: &[(LocalId, usize)], span: Span) {
        self.current = stop.block;
        if let Some(block) = self.blocks.get_mut(stop.block.0) {
            block.end = None;
        }
        self.set_slot(slot::ITEM, stop.value.clone(), span);
        for (local, at) in saved {
            self.set_slot(*at, Operand::Local(*local), span);
        }
        self.set_slot(slot::PC, Operand::Int(pc), span);
        self.finish(Terminator::Return(Some(Operand::Bool(true))));
    }

    /// Makes each way out of the body say there's nothing more.
    fn finish_returns(&mut self, span: Span) {
        for at in 0..self.blocks.len() {
            let block = crate::BlockId(at);
            let end = self.blocks.get(at).and_then(|open| open.end.clone());
            let Some(Terminator::Return(value)) = end else { continue };
            if value == Some(Operand::Bool(true)) {
                continue;
            }
            if let Some(open) = self.blocks.get_mut(at) {
                open.end = None;
            }
            self.current = block;
            match value {
                Some(result) => self.finish_with_result(result, span),
                None => {
                    self.set_slot(slot::PC, Operand::Int(FINISHED), span);
                    self.finish(Terminator::Return(Some(Operand::Bool(false))));
                }
            }
        }
    }

    /// Leaves a generator that can fail: a failure goes in its block for whatever asked.
    fn finish_with_result(&mut self, result: Operand, span: Span) {
        let failed = self.temporary(Type::Bool);
        self.emit(InstKind::IsErr { dest: failed, value: result.clone() }, span);
        let (then, otherwise) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(failed), then, otherwise });
        self.current = then;
        let error = match &self.returns {
            Type::Result(_, error) => (**error).clone(),
            _ => Type::Unit,
        };
        let held = self.temporary(error);
        self.emit(InstKind::GetErr { dest: held, value: result }, span);
        self.set_slot(slot::FAILURE, Operand::Local(held), span);
        self.set_slot(slot::PC, Operand::Int(FAILED), span);
        self.finish(Terminator::Return(Some(Operand::Bool(false))));
        self.current = otherwise;
        self.set_slot(slot::PC, Operand::Int(FINISHED), span);
        self.finish(Terminator::Return(Some(Operand::Bool(false))));
    }

    /// The first block: reads where to go on from, and goes there, with what it kept back.
    fn choose_where_to_go_on(&mut self, places: &[(i64, crate::BlockId, Vec<(LocalId, usize)>)], span: Span) {
        let this = Operand::Local(LocalId(0));
        self.current = crate::BlockId(0);
        if let Some(block) = self.blocks.get_mut(0) {
            block.end = None;
        }
        let pc = self.temporary(Type::Int);
        self.emit(InstKind::GetSlot { dest: pc, generator: this.clone(), slot: slot::PC }, span);
        self.branch_if_pc(pc, RUNNING, span, |lowering| {
            lowering.finish(Terminator::Stop { message: "this generator is already running".to_owned(), span });
        });
        for ended in [FINISHED, FAILED] {
            self.branch_if_pc(pc, ended, span, |lowering| {
                lowering.finish(Terminator::Return(Some(Operand::Bool(false))));
            });
        }
        for (place, next, saved) in places {
            self.branch_if_pc(pc, *place, span, |lowering| {
                lowering.set_slot(slot::PC, Operand::Int(RUNNING), span);
                for (local, at) in saved {
                    let kind = InstKind::GetSlot { dest: *local, generator: this.clone(), slot: *at };
                    lowering.emit(kind, span);
                }
                lowering.finish(Terminator::Jump(*next));
            });
        }
        self.finish(Terminator::Unreachable);
    }

    /// When `pc` holds this, does `then`, which ends its block. Otherwise carries on.
    fn branch_if_pc(&mut self, pc: LocalId, value: i64, span: Span, then: impl FnOnce(&mut Self)) {
        let same = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(pc), Operand::Int(value));
        self.emit(InstKind::Binary { dest: same, op: BinOp::Eq, lhs, rhs }, span);
        let (here, rest) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(same), then: here, otherwise: rest });
        self.current = here;
        then(self);
        self.current = rest;
    }

    /// `(elem for names in iter if cond)` as a value: a function of its own, given what the
    /// first `in` gives now and what it mentions.
    pub(super) fn generator_expression(
        &mut self,
        expr: &Expr,
        (elem, cond): (&Expr, Option<&Expr>),
        (names, iter): (&[String], &Expr),
    ) -> Lowered<Operand> {
        const SOURCE: &str = "\0source";
        let ty = self.ty(expr)?;
        let Type::Iterator(item) = &ty else { return Err(format!("a generator expression is {ty}")) };
        let source_ty = self.ty(iter)?;
        let source_value = self.expr(iter)?;
        let mut mentioned = Vec::new();
        for written in std::iter::once(elem).chain(cond) {
            let mut written = written.clone();
            fennec_syntax::visit::in_expr(&mut written, &mut |seen| {
                if let ExprKind::Name(name) = &seen.kind {
                    mentioned.push(name.clone());
                }
            });
        }
        let own: Vec<&str> = names.iter().map(String::as_str).collect();
        let captures = self.captures(mentioned, &own);
        let key = format!("{}.generator@{}", self.name, expr.span.start);
        let instance = FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut lifted = Lowering::new(&instance, self.context, self.body, Type::Unit);
        lifted.begin_generator((**item).clone());
        let mut params = Vec::new();
        for (name, local) in &captures {
            params.push(lifted.declare(name, self.local_type(*local)));
        }
        params.push(lifted.declare(SOURCE, source_ty));
        let span = expr.span;
        let yielded = Stmt { kind: StmtKind::Yield(elem.clone()), span };
        let body = match cond {
            Some(cond) => vec![Stmt {
                kind: StmtKind::If { branches: vec![(cond.clone(), vec![yielded])], otherwise: None },
                span,
            }],
            None => vec![yielded],
        };
        let source = Expr { kind: ExprKind::Name(SOURCE.to_owned()), span: iter.span };
        let looping = StmtKind::For { names: names.to_vec(), is_mut: false, iter: source, body };
        lifted.stmts(&[Stmt { kind: looping, span }], Tail::Drop)?;
        lifted.end_function(span)?;
        self.lifted.extend(lifted.finish_generator(&params, &ty, span)?);
        let mut args: Vec<Operand> = captures.iter().map(|(_, local)| Operand::Local(*local)).collect();
        args.push(source_value);
        let dest = self.temporary(ty);
        self.emit(InstKind::Call { dest: Some(dest), function: key, args }, span);
        Ok(Operand::Local(dest))
    }

    /// `enumerate(items)` of a generator, as a generator.
    pub(super) fn enumerated_generator(&mut self, expr: &Expr, items: &Expr) -> Lowered<Operand> {
        const SOURCE: &str = "\0source";
        let ty = self.ty(expr)?;
        let Type::Iterator(pair) = &ty else { return Err(format!("`enumerate` gives {ty}")) };
        let Type::Tuple(parts) = &**pair else { return Err(format!("`enumerate` gives {ty}")) };
        let span = expr.span;
        let source_ty = self.ty(items)?;
        let source_value = self.expr(items)?;
        let key = format!("{}.enumerate@{}", self.name, span.start);
        let instance = FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut lifted = Lowering::new(&instance, self.context, self.body, Type::Unit);
        lifted.begin_generator((**pair).clone());
        let source = lifted.declare(SOURCE, source_ty);
        let count = lifted.temporary(Type::Int);
        lifted.emit(InstKind::Copy { dest: count, value: Operand::Int(0) }, span);
        let each = "\0each".to_owned();
        let names = [each.clone()];
        let read = Expr { kind: ExprKind::Name(SOURCE.to_owned()), span: items.span };
        let tuple_ty = Type::Tuple(parts.clone());
        lifted.looping(Over { names: &names, iter: &read, back: None }, span, |inner| {
            let item = inner.lookup(&each).ok_or("`enumerate` lost its item")?;
            let dest = inner.temporary(tuple_ty);
            let fields = vec![Operand::Local(count), Operand::Local(item)];
            inner.emit(InstKind::MakeTuple { dest, items: fields }, span);
            let lhs = Operand::Local(count);
            inner.emit(InstKind::Binary { dest: count, op: BinOp::Add, lhs, rhs: Operand::Int(1) }, span);
            inner.yield_operand(Operand::Local(dest));
            Ok(())
        })?;
        lifted.end_function(span)?;
        self.lifted.extend(lifted.finish_generator(&[source], &ty, span)?);
        let dest = self.temporary(ty);
        self.emit(InstKind::Call { dest: Some(dest), function: key, args: vec![source_value] }, span);
        Ok(Operand::Local(dest))
    }

    /// Runs `body` for each item a generator gives, failing if it's been used already.
    pub(super) fn through_generator<F>(
        &mut self,
        names: &[String],
        (generator, item): (Operand, Type),
        known: Option<String>,
        span: Span,
        body: F,
    ) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        self.start_generator(&generator, span);
        let (head, then, done, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Jump(head));
        self.current = head;
        let more = self.temporary(Type::Bool);
        let generator_read = generator.clone();
        self.emit(InstKind::Resume { dest: more, generator: generator_read, known }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(more), then, otherwise: done });
        self.current = then;
        self.scopes.push(HashMap::new());
        let held = self.temporary(item);
        self.emit(InstKind::GetSlot { dest: held, generator: generator.clone(), slot: slot::ITEM }, span);
        let bound = self.bind_generated(names, held, span);
        self.loops.push(Loop { next: head, exit, depth: self.leaving.len() });
        let lowered = bound.and_then(|()| body(self));
        self.loops.pop();
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(head));
        self.current = done;
        self.pass_up_failure(&generator, exit, span)?;
        self.current = exit;
        Ok(())
    }

    /// A generator can be used once.
    fn start_generator(&mut self, generator: &Operand, span: Span) {
        let started = self.temporary(Type::Bool);
        self.emit(InstKind::GetSlot { dest: started, generator: generator.clone(), slot: slot::STARTED }, span);
        let (then, otherwise) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(started), then, otherwise });
        self.current = then;
        self.finish(Terminator::Stop { message: "this generator has already been used".to_owned(), span });
        self.current = otherwise;
        let kind = InstKind::SetSlot { generator: generator.clone(), slot: slot::STARTED, value: Operand::Bool(true) };
        self.emit(kind, span);
    }

    fn bind_generated(&mut self, names: &[String], held: LocalId, span: Span) -> Lowered<()> {
        if let [name] = names {
            let ty = self.local_type(held);
            let dest = self.declare(name, ty);
            self.emit(InstKind::Copy { dest, value: Operand::Local(held) }, span);
            return Ok(());
        }
        let Type::Tuple(types) = self.local_type(held) else {
            return Err("it unpacks something other than a tuple".to_owned());
        };
        for (at, (name, ty)) in names.iter().zip(types).enumerate() {
            let dest = self.declare(name, ty);
            self.emit(InstKind::GetTupleItem { dest, value: Operand::Local(held), at }, span);
        }
        Ok(())
    }

    /// After a generator's last item: a failure passes up from here, as from any call. One in
    /// a function that can't pass it up ends the program.
    fn pass_up_failure(&mut self, generator: &Operand, exit: crate::BlockId, span: Span) -> Lowered<()> {
        let pc = self.temporary(Type::Int);
        self.emit(InstKind::GetSlot { dest: pc, generator: generator.clone(), slot: slot::PC }, span);
        let failed = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(pc), Operand::Int(FAILED));
        self.emit(InstKind::Binary { dest: failed, op: BinOp::Eq, lhs, rhs }, span);
        let then = self.new_block();
        self.finish(Terminator::Branch { cond: Operand::Local(failed), then, otherwise: exit });
        self.current = then;
        let Type::Result(_, error) = self.returns.clone() else {
            let message = "this function can't pass up a generator's failure".to_owned();
            self.finish(Terminator::Stop { message, span });
            return Ok(());
        };
        let held = self.temporary(*error);
        self.emit(InstKind::GetSlot { dest: held, generator: generator.clone(), slot: slot::FAILURE }, span);
        let dest = self.temporary(self.returns.clone());
        self.emit(InstKind::MakeErr { dest, error: Operand::Local(held) }, span);
        self.leave(Some(Operand::Local(dest)), span)
    }
}

/// The function a call to a yield function runs: it makes the block with what the body needs
/// from its parameters, and gives it back.
fn make_block_function(
    name: &str,
    params: &[LocalId],
    slots: &HashMap<LocalId, usize>,
    (returns, _types): (&Type, &[Type]),
    functions: &[Function],
    span: Span,
) -> Function {
    let resume_locals = functions.first().map_or(&[][..], |resume| resume.locals.as_slice());
    let mut locals: Vec<Local> = params
        .iter()
        .filter_map(|param| resume_locals.get(param.0))
        .map(|local| Local { name: local.name.clone(), ty: local.ty.clone() })
        .collect();
    let args: Vec<(usize, Operand)> = params
        .iter()
        .enumerate()
        .filter_map(|(at, param)| slots.get(param).map(|slot| (*slot, Operand::Local(LocalId(at)))))
        .collect();
    let item = match returns {
        Type::Result(ok, _) => (**ok).clone(),
        other => other.clone(),
    };
    let made = LocalId(locals.len());
    locals.push(Local { name: None, ty: item });
    let mut insts =
        vec![Inst { kind: InstKind::MakeGenerator { dest: made, generator: name.to_owned(), args }, span }];
    let mut result = made;
    if matches!(returns, Type::Result(..)) {
        result = LocalId(locals.len());
        locals.push(Local { name: None, ty: returns.clone() });
        insts.push(Inst { kind: InstKind::MakeOk { dest: result, value: Some(Operand::Local(made)) }, span });
    }
    Function {
        name: name.to_owned(),
        params: params.len(),
        mut_params: Vec::new(),
        returns: returns.clone(),
        locals,
        blocks: vec![crate::Block { insts, end: Terminator::Return(Some(Operand::Local(result))) }],
        counts: Vec::new(),
    }
}
