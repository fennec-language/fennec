//! The IR as text, for tests and for reading.

use std::fmt;

use fennec_check::Type;
use fennec_syntax::UnaryOp;

use crate::{BlockId, Function, InstKind, LocalId, Operand, Program, Terminator};

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for function in &self.functions {
            writeln!(f, "{function}")?;
        }
        for skipped in &self.not_lowered {
            writeln!(f, "# not lowered: {}: {}", skipped.function, skipped.why)?;
        }
        Ok(())
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let params: Vec<String> = (0..self.params).map(|at| LocalId(at).to_string()).collect();
        write!(f, "fn {}({})", self.name, params.join(", "))?;
        if self.returns != Type::Unit {
            write!(f, " -> {}", self.returns)?;
        }
        writeln!(f)?;
        for (at, local) in self.locals.iter().enumerate() {
            let name = local.name.as_ref().map_or(String::new(), |name| format!(" {name}"));
            writeln!(f, "  local {}{name}: {}", LocalId(at), local.ty)?;
        }
        for (at, block) in self.blocks.iter().enumerate() {
            writeln!(f, "  {}:", BlockId(at))?;
            for inst in &block.insts {
                writeln!(f, "    {}", inst.kind)?;
            }
            writeln!(f, "    {}", block.end)?;
        }
        Ok(())
    }
}

impl fmt::Display for LocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "_{}", self.0)
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Local(id) => write!(f, "{id}"),
            Operand::Int(value) => write!(f, "{value}"),
            Operand::Float(value) => write!(f, "{value:?}"),
            Operand::Bool(value) => write!(f, "{value}"),
            Operand::Str(value) => write!(f, "{value:?}"),
        }
    }
}

impl fmt::Display for InstKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstKind::Copy { dest, value } => write!(f, "{dest} = {value}"),
            InstKind::Unary { dest, op: UnaryOp::Neg, value } => write!(f, "{dest} = -{value}"),
            InstKind::Unary { dest, op: UnaryOp::Not, value } => write!(f, "{dest} = not {value}"),
            InstKind::Binary { dest, op, lhs, rhs } => write!(f, "{dest} = {lhs} {} {rhs}", op.symbol()),
            InstKind::Call { dest, function, args } => {
                let args: Vec<String> = args.iter().map(ToString::to_string).collect();
                if let Some(dest) = dest {
                    write!(f, "{dest} = ")?;
                }
                write!(f, "call {function}({})", args.join(", "))
            }
        }
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Jump(target) => write!(f, "jump {target}"),
            Terminator::Branch { cond, then, otherwise } => write!(f, "branch {cond} {then} {otherwise}"),
            Terminator::Return(Some(value)) => write!(f, "return {value}"),
            Terminator::Return(None) => write!(f, "return"),
            Terminator::Unreachable => write!(f, "unreachable"),
        }
    }
}
