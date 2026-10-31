//! Lowering a function from the typed IR rather than the syntax tree. This grows a kind of
//! function at a time: a function the IR path doesn't take yet stays on the syntax tree, so
//! the two work side by side and call each other the same way.
//!
//! So far it takes functions that only hold ints, floats and bools. It leaves a function
//! calling something small enough to inline on the syntax tree, since the IR has no inlining
//! until the optimizer does it.

use cranelift_codegen::ir::{InstBuilder, Value, types};
use cranelift_frontend::Variable;
use fennec_check::Type;
use fennec_ir::{BlockId, Function, Inst, InstKind, LocalId, Operand, Terminator};
use fennec_syntax::{BinOp, Diagnostic, Span};

use super::{Lowerer, Shared, internal};
use crate::types::Ty;

/// How native code holds a number or bool of this type.
fn plain(ty: &Type) -> Option<Ty> {
    match ty {
        Type::Int => Some(Ty::Int),
        Type::Float => Some(Ty::Float),
        Type::Bool => Some(Ty::Bool),
        _ => None,
    }
}

/// Whether the IR path can lower this instruction. A call has to be to a function that's
/// called the plain way, with a value for each parameter.
fn takes(inst: &InstKind, function: &Function, shared: &Shared) -> bool {
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
        InstKind::Call { function: callee, args, .. } => {
            shared.compiled.get(callee).is_some_and(|compiled| {
                compiled.inline.is_none()
                    && compiled.specialized.is_none()
                    && compiled.direct.is_none()
                    && compiled.params.len() == args.len()
                    && compiled
                        .params
                        .iter()
                        .all(|(_, ty, is_mut)| !is_mut && matches!(ty, Ty::Int | Ty::Float | Ty::Bool))
                    && matches!(compiled.ret, Ty::Int | Ty::Float | Ty::Bool | Ty::Unit)
            })
        }
        _ => false,
    }
}

/// Whether the IR path can lower a function.
pub(super) fn taken(function: &Function, written: &crate::types::Function<'_>, shared: &Shared) -> bool {
    let locals = function.locals.iter().all(|local| plain(&local.ty).is_some());
    let returns = function.returns == Type::Unit || plain(&function.returns).is_some();
    let ends = function.blocks.iter().all(|block| {
        matches!(
            block.end,
            Terminator::Jump(_) | Terminator::Branch { .. } | Terminator::Return(_) | Terminator::Unreachable
        )
    });
    let mut insts = function.blocks.iter().flat_map(|block| &block.insts);
    !written.fallible && locals && returns && ends && insts.all(|inst| takes(&inst.kind, function, shared))
}

impl Lowerer<'_, '_> {
    /// Lowers a function's body from its IR. Its parameters are already bound.
    pub(super) fn body_from_ir(&mut self, ir: &Function) -> Result<(), Diagnostic> {
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
        let blocks: Vec<_> = ir.blocks.iter().map(|_| self.b.create_block()).collect();
        let block_of = |id: BlockId| blocks.get(id.0).copied();
        let first = block_of(BlockId(0)).ok_or_else(|| internal("a function has no blocks", span))?;
        self.b.ins().jump(first, &[]);
        for (block, written) in blocks.iter().zip(&ir.blocks) {
            self.b.switch_to_block(*block);
            for inst in &written.insts {
                self.inst_from_ir(inst, &vars)?;
            }
            match &written.end {
                Terminator::Jump(target) => {
                    let target = block_of(*target).ok_or_else(|| internal("no such block", span))?;
                    self.b.ins().jump(target, &[]);
                }
                Terminator::Branch { cond, then, otherwise } => {
                    let cond = self.operand_from_ir(cond, &vars, span)?;
                    let then = block_of(*then).ok_or_else(|| internal("no such block", span))?;
                    let otherwise = block_of(*otherwise).ok_or_else(|| internal("no such block", span))?;
                    self.b.ins().brif(cond, then, &[], otherwise, &[]);
                }
                Terminator::Return(value) => {
                    let value =
                        value.as_ref().map(|value| self.operand_from_ir(value, &vars, span)).transpose()?;
                    let ty = plain(&ir.returns).unwrap_or(Ty::Unit);
                    self.ret(value, ty, span)?;
                }
                // The checker has shown control never gets here.
                _ => {
                    self.b.ins().jump(self.bail, &[]);
                }
            }
        }
        self.shared.from_ir.push(self.function.key.clone());
        Ok(())
    }

    fn inst_from_ir(&mut self, inst: &Inst, vars: &[(Variable, Ty)]) -> Result<(), Diagnostic> {
        let span = inst.span;
        match &inst.kind {
            InstKind::Copy { dest, value } => {
                let value = self.operand_from_ir(value, vars, span)?;
                self.set_from_ir(*dest, value, vars, span)
            }
            InstKind::Unary { dest, op, value } => {
                let ty = self.operand_type(value, vars, span)?;
                let value = self.operand_from_ir(value, vars, span)?;
                let result = self.unary(*op, (Some(value), ty), span)?;
                let value = result.and_then(|(value, _)| value);
                let value = value.ok_or_else(|| internal("an operator gave nothing", span))?;
                self.set_from_ir(*dest, value, vars, span)
            }
            InstKind::Binary { dest, op, lhs, rhs } => {
                let ty = self.operand_type(lhs, vars, span)?;
                let (lhs, rhs) =
                    (self.operand_from_ir(lhs, vars, span)?, self.operand_from_ir(rhs, vars, span)?);
                let result = self.binary(*op, (Some(lhs), ty), (Some(rhs), ty), span);
                let value = result.and_then(|(value, _)| value);
                let value = value.ok_or_else(|| internal("an operator gave nothing", span))?;
                self.set_from_ir(*dest, value, vars, span)
            }
            InstKind::Call { dest, function, args } => {
                let values =
                    args.iter()
                        .map(|arg| self.operand_from_ir(arg, vars, span))
                        .collect::<Result<Vec<Value>, Diagnostic>>()?;
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
        vars: &[(Variable, Ty)],
        span: Span,
    ) -> Result<(), Diagnostic> {
        let (var, _) = vars.get(dest.0).ok_or_else(|| internal("no such local", span))?;
        self.b.def_var(*var, value);
        Ok(())
    }

    fn operand_type(&self, operand: &Operand, vars: &[(Variable, Ty)], span: Span) -> Result<Ty, Diagnostic> {
        match operand {
            Operand::Local(local) => {
                vars.get(local.0).map(|(_, ty)| *ty).ok_or_else(|| internal("no such local", span))
            }
            Operand::Int(_) => Ok(Ty::Int),
            Operand::Float(_) => Ok(Ty::Float),
            Operand::Bool(_) => Ok(Ty::Bool),
            Operand::Str(_) | Operand::None => Err(internal("a value the IR path doesn't take", span)),
        }
    }

    fn operand_from_ir(
        &mut self,
        operand: &Operand,
        vars: &[(Variable, Ty)],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match operand {
            Operand::Local(local) => {
                let (var, _) = vars.get(local.0).ok_or_else(|| internal("no such local", span))?;
                Ok(self.b.use_var(*var))
            }
            Operand::Int(value) => Ok(self.int(*value)),
            Operand::Float(value) => Ok(self.b.ins().f64const(*value)),
            Operand::Bool(value) => Ok(self.b.ins().iconst(types::I8, i64::from(*value))),
            Operand::Str(_) | Operand::None => Err(internal("a value the IR path doesn't take", span)),
        }
    }
}
