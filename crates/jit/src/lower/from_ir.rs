//! Lowering a function from the typed IR rather than the syntax tree. This grows a kind of
//! function at a time: a function the IR path doesn't take yet stays on the syntax tree, so
//! the two work side by side and call each other the same way.
//!
//! So far it takes functions that only hold ints, floats and bools. It leaves a function on
//! the syntax tree when that would make it slower: one calling something small enough to
//! inline that the IR path can't lower too. What it can lower it inlines the same way, until
//! the optimizer does that on the IR.
//!
//! What's known about an int comes from `fennec_ir::facts`, which any backend can use.

use cranelift_codegen::ir::{Block, InstBuilder, Value, types};
use cranelift_frontend::Variable;
use fennec_check::Type;
use fennec_ir::facts::Facts;
use fennec_ir::{BlockId, Function, Inst, InstKind, LocalId, Operand, Terminator};
use fennec_syntax::{BinOp, Diagnostic, Span};

use super::{Lowerer, Shared, internal};
use crate::types::Ty;

/// A function's locals as the IR path lowers it.
struct Locals {
    vars: Vec<(Variable, Ty)>,
    facts: Facts,
    /// The block being lowered.
    block: usize,
}

impl Locals {
    fn var(&self, local: LocalId, span: Span) -> Result<(Variable, Ty), Diagnostic> {
        self.vars.get(local.0).copied().ok_or_else(|| internal("no such local", span))
    }
}

/// The facts of a function being lowered: what it passes as `mut` is what the JIT compiled.
fn facts_of(ir: &Function, shared: &Shared) -> Facts {
    Facts::of(ir, &|callee| {
        let params = shared.compiled.get(callee).map_or(&[][..], |compiled| compiled.params.as_slice());
        params.iter().enumerate().filter(|(_, (_, _, is_mut))| *is_mut).map(|(at, _)| at).collect()
    })
}

/// How native code holds a number or bool of this type.
fn plain(ty: &Type) -> Option<Ty> {
    match ty {
        Type::Int => Some(Ty::Int),
        Type::Float => Some(Ty::Float),
        Type::Bool => Some(Ty::Bool),
        _ => None,
    }
}

/// The name the JIT knows a builtin on numbers by.
fn number_builtin_name(name: &str) -> &str {
    match name {
        "int.abs" | "float.abs" => "abs",
        _ => name.strip_prefix("math.").unwrap_or(name),
    }
}

/// Whether the IR path can lower this instruction. A call has to be to a function that's
/// called the plain way, with a value for each parameter. One small enough to inline has to be
/// one the IR path can lower too, or the call would cost more than it does on the syntax tree.
/// `open` holds the functions being looked at, which are fine to call if they get that far.
fn takes(inst: &InstKind, function: &Function, shared: &Shared, open: &mut Vec<String>) -> bool {
    match inst {
        InstKind::Copy { .. } => true,
        InstKind::Unary { value, .. } => {
            function.type_of(value).as_ref().is_some_and(|ty| plain(ty).is_some())
        }
        InstKind::Binary { op, lhs, .. } => match function.type_of(lhs) {
            Some(Type::Int | Type::Float) => true,
            Some(Type::Bool) => matches!(op, BinOp::Eq | BinOp::NotEq),
            _ => false,
        },
        InstKind::Builtin { dest: Some(_), name, args, changes: false } => {
            let types: Vec<_> = args.iter().map(|arg| function.type_of(arg)).collect();
            matches!(
                (number_builtin_name(name), types.as_slice()),
                ("min" | "max", [Some(Type::Int), Some(Type::Int)] | [Some(Type::Float), Some(Type::Float)])
                    | (
                        "float" | "int" | "sqrt" | "floor" | "ceil" | "sin" | "cos" | "abs",
                        [Some(Type::Int | Type::Float)]
                    )
            )
        }
        InstKind::Call { function: callee, args, .. } => {
            let Some(compiled) = shared.compiled.get(callee) else { return false };
            // Lowering a call for what it's passed is for arrows and generics, not numbers.
            let plain_call = compiled.direct.is_none()
                && compiled.params.len() == args.len()
                && compiled
                    .params
                    .iter()
                    .all(|(_, ty, is_mut)| !is_mut && matches!(ty, Ty::Int | Ty::Float | Ty::Bool))
                && matches!(compiled.ret, Ty::Int | Ty::Float | Ty::Bool | Ty::Unit);
            let mut inlines = || {
                compiled.inline.is_none()
                    || open.contains(callee)
                    || shared.ir.get(callee).is_some_and(|ir| body_taken(ir, shared, open))
            };
            plain_call && inlines()
        }
        _ => false,
    }
}

/// Whether the IR path can lower a function that can't fail.
fn body_taken(function: &Function, shared: &Shared, open: &mut Vec<String>) -> bool {
    open.push(function.name.clone());
    let locals = function.locals.iter().all(|local| plain(&local.ty).is_some());
    let returns = function.returns == Type::Unit || plain(&function.returns).is_some();
    let ends = function.blocks.iter().all(|block| {
        matches!(
            block.end,
            Terminator::Jump(_) | Terminator::Branch { .. } | Terminator::Return(_) | Terminator::Unreachable
        )
    });
    let mut insts = function.blocks.iter().flat_map(|block| &block.insts);
    let taken = locals && returns && ends && insts.all(|inst| takes(&inst.kind, function, shared, open));
    open.pop();
    taken
}

/// Whether the IR path can lower a function.
pub(super) fn taken(function: &Function, written: &crate::types::Function<'_>, shared: &Shared) -> bool {
    !written.fallible && body_taken(function, shared, &mut Vec::new())
}

/// Where an inlined function's `return` goes: what holds its value, and where to carry on.
#[derive(Clone, Copy)]
struct Leave {
    value: Option<Variable>,
    after: Block,
}

impl Lowerer<'_, '_> {
    /// Lowers a function's body, from its IR when the IR path takes it.
    pub(super) fn body(&mut self) -> Result<(), Diagnostic> {
        let function = self.function;
        let ir = std::rc::Rc::clone(&self.shared.ir);
        if let Some(ir) = ir.get(&function.key).filter(|ir| taken(ir, function, self.shared)) {
            return self.body_from_ir(ir);
        }
        let (tail, tail_ty) = self.tail(&function.def.body)?;
        if !self.is_dead() {
            self.ret(tail, tail_ty, function.def.span)?;
        }
        Ok(())
    }

    /// Lowers a function's body from its IR. Its parameters are already bound.
    fn body_from_ir(&mut self, ir: &Function) -> Result<(), Diagnostic> {
        let mut vars = Vec::with_capacity(ir.locals.len());
        let span = self.function.def.span;
        for (at, local) in ir.locals.iter().enumerate() {
            let ty = plain(&local.ty).ok_or_else(|| internal("a local isn't a number", span))?;
            let param = self.function.params.get(at).filter(|_| at < ir.params);
            let var = match param {
                Some(param) => self.local_param(&param.name)?.var,
                None => Some(self.b.declare_var(super::abi(ty).unwrap_or(types::I64))),
            };
            vars.push((var.ok_or_else(|| internal("a parameter has no variable", span))?, ty));
        }
        let mut vars = Locals { vars, facts: facts_of(ir, self.shared), block: 0 };
        self.blocks_from_ir(ir, &mut vars, None, span)?;
        self.shared.from_ir.push(self.function.key.clone());
        Ok(())
    }

    /// Lowers a function's blocks, going to the first from here. Leaving it returns from this
    /// function, unless `leave` says it's being lowered into a caller.
    fn blocks_from_ir(
        &mut self,
        ir: &Function,
        vars: &mut Locals,
        leave: Option<Leave>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let blocks: Vec<_> = ir.blocks.iter().map(|_| self.b.create_block()).collect();
        let block_of = |id: BlockId| blocks.get(id.0).copied();
        let first = block_of(BlockId(0)).ok_or_else(|| internal("a function has no blocks", span))?;
        self.b.ins().jump(first, &[]);
        for (at, (block, written)) in blocks.iter().zip(&ir.blocks).enumerate() {
            self.b.switch_to_block(*block);
            vars.block = at;
            for inst in &written.insts {
                self.inst_from_ir(inst, vars)?;
            }
            match &written.end {
                Terminator::Jump(target) => {
                    let target = block_of(*target).ok_or_else(|| internal("no such block", span))?;
                    self.b.ins().jump(target, &[]);
                }
                Terminator::Branch { cond, then, otherwise } => {
                    let cond = self.operand_from_ir(cond, vars, span)?;
                    let then = block_of(*then).ok_or_else(|| internal("no such block", span))?;
                    let otherwise = block_of(*otherwise).ok_or_else(|| internal("no such block", span))?;
                    self.b.ins().brif(cond, then, &[], otherwise, &[]);
                }
                Terminator::Return(value) => {
                    let value =
                        value.as_ref().map(|value| self.operand_from_ir(value, vars, span)).transpose()?;
                    if let Some(Leave { value: held, after }) = leave {
                        if let (Some(held), Some(value)) = (held, value) {
                            self.b.def_var(held, value);
                        }
                        self.b.ins().jump(after, &[]);
                    } else {
                        let ty = plain(&ir.returns).unwrap_or(Ty::Unit);
                        self.ret(value, ty, span)?;
                    }
                }
                // The checker has shown control never gets here.
                _ => {
                    self.b.ins().jump(self.bail, &[]);
                }
            }
        }
        Ok(())
    }

    /// Lowers a call by lowering the function's body here, as the syntax tree path does, with
    /// its parameters holding `values`. Gives back what it gives back.
    fn inline_from_ir(
        &mut self,
        (key, def, ir): (&str, &fennec_syntax::FnDef, &Function),
        values: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Diagnostic> {
        self.inlining.push((key.to_owned(), self.scopes.len()));
        let outer_site = self.call_site;
        if !def.span.in_prelude() {
            self.mark_ran(key)?;
        }
        let outer_origin = self.enter_origin(key, def);
        if self.call_site.is_none() && !span.in_prelude() {
            self.call_site = Some(span);
        }
        let mut vars = Vec::with_capacity(ir.locals.len());
        for local in &ir.locals {
            let ty = plain(&local.ty).ok_or_else(|| internal("a local isn't a number", span))?;
            vars.push((self.b.declare_var(super::abi(ty).unwrap_or(types::I64)), ty));
        }
        let mut facts = facts_of(ir, self.shared);
        for (at, (value, (var, ty))) in values.iter().zip(&vars).enumerate() {
            self.b.def_var(*var, *value);
            if *ty == Ty::Int {
                facts.note_param(LocalId(at), self.range(*value));
            }
        }
        let mut vars = Locals { vars, facts, block: 0 };
        let held = plain(&ir.returns).map(|ty| self.b.declare_var(super::abi(ty).unwrap_or(types::I64)));
        let after = self.b.create_block();
        let lowered = self.blocks_from_ir(ir, &mut vars, Some(Leave { value: held, after }), span);
        self.inlining.pop();
        self.call_site = outer_site;
        self.inlined_origin = outer_origin;
        lowered?;
        self.b.switch_to_block(after);
        Ok(held.map(|held| self.b.use_var(held)))
    }

    fn inst_from_ir(&mut self, inst: &Inst, vars: &mut Locals) -> Result<(), Diagnostic> {
        let span = inst.span;
        match &inst.kind {
            InstKind::Copy { dest, value } => {
                let value = self.operand_from_ir(value, vars, span)?;
                self.set_from_ir(*dest, value, vars, span)
            }
            InstKind::Unary { dest, op, value } => {
                let ty = operand_type(value, vars, span)?;
                let value = self.operand_from_ir(value, vars, span)?;
                let result = self.unary(*op, (Some(value), ty), span)?;
                let value = result.and_then(|(value, _)| value);
                let value = value.ok_or_else(|| internal("an operator gave nothing", span))?;
                self.set_from_ir(*dest, value, vars, span)
            }
            InstKind::Binary { dest, op, lhs, rhs } => {
                let ty = operand_type(lhs, vars, span)?;
                let (lhs, rhs) =
                    (self.operand_from_ir(lhs, vars, span)?, self.operand_from_ir(rhs, vars, span)?);
                let result = self.binary(*op, (Some(lhs), ty), (Some(rhs), ty), span);
                let value = result.and_then(|(value, _)| value);
                let value = value.ok_or_else(|| internal("an operator gave nothing", span))?;
                self.set_from_ir(*dest, value, vars, span)
            }
            InstKind::Builtin { dest: Some(dest), name, args, .. } => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    let ty = operand_type(arg, vars, span)?;
                    values.push((Some(self.operand_from_ir(arg, vars, span)?), ty));
                }
                let name = number_builtin_name(name);
                let result = match (name, values.first()) {
                    ("abs", Some(only)) => self.absolute_number(*only, span),
                    _ => self.number_builtin(name, &values, span)?,
                };
                let (value, _) =
                    result.ok_or_else(|| internal("a builtin the IR path doesn't take", span))?;
                self.set_from_ir(*dest, value, vars, span)
            }
            InstKind::Call { dest, function, args } => {
                let values =
                    args.iter()
                        .map(|arg| self.operand_from_ir(arg, vars, span))
                        .collect::<Result<Vec<Value>, Diagnostic>>()?;
                if let Some(def) = self.inlinable(function) {
                    let all = std::rc::Rc::clone(&self.shared.ir);
                    let ir = all.get(function).ok_or_else(|| internal("no IR to inline", span))?;
                    let result = self.inline_from_ir((function, &def, ir), &values, span)?;
                    if let (Some(dest), Some(result)) = (dest, result) {
                        self.set_from_ir(*dest, result, vars, span)?;
                    }
                    return Ok(());
                }
                let compiled = self.shared.compiled.get(function);
                let compiled = compiled.ok_or_else(|| internal("a call to nothing", span))?;
                let (id, prelude) = (compiled.id, compiled.prelude);
                self.shared.called.insert(function.clone());
                let results = self.raw_call(id, &values, prelude, span);
                if let (Some(dest), Some(result)) = (dest, results.first()) {
                    self.set_from_ir(*dest, *result, vars, span)?;
                }
                Ok(())
            }
            _ => Err(internal("an instruction the IR path doesn't take", span)),
        }
    }

    fn set_from_ir(
        &mut self,
        dest: LocalId,
        value: Value,
        vars: &mut Locals,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let (var, ty) = vars.var(dest, span)?;
        if ty == Ty::Int {
            vars.facts.note_write(dest, self.range(value));
        }
        self.b.def_var(var, value);
        Ok(())
    }

    fn operand_from_ir(&mut self, operand: &Operand, vars: &Locals, span: Span) -> Result<Value, Diagnostic> {
        match operand {
            Operand::Local(local) => {
                let (var, _) = vars.var(*local, span)?;
                let value = self.b.use_var(var);
                if let Some(range) = vars.facts.range_in(*local, vars.block) {
                    self.note_range(value, range);
                }
                Ok(value)
            }
            Operand::Int(value) => Ok(self.int(*value)),
            Operand::Float(value) => Ok(self.b.ins().f64const(*value)),
            Operand::Bool(value) => Ok(self.b.ins().iconst(types::I8, i64::from(*value))),
            Operand::Str(_) | Operand::None => Err(internal("a value the IR path doesn't take", span)),
        }
    }
}

fn operand_type(operand: &Operand, vars: &Locals, span: Span) -> Result<Ty, Diagnostic> {
    match operand {
        Operand::Local(local) => vars.var(*local, span).map(|(_, ty)| ty),
        Operand::Int(_) => Ok(Ty::Int),
        Operand::Float(_) => Ok(Ty::Float),
        Operand::Bool(_) => Ok(Ty::Bool),
        Operand::Str(_) | Operand::None => Err(internal("a value the IR path doesn't take", span)),
    }
}
