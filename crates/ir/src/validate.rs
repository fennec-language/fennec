//! Checks the IR is well formed, so a mistake in lowering is caught here and not as a
//! miscompiled program. Passing says nothing about what the source meant, only that every
//! local and block exists and every instruction's types line up.

use fennec_check::Type;
use fennec_syntax::{BinOp, UnaryOp};

use crate::{Function, InstKind, LocalId, Operand, Program, Terminator};

/// Every way a program's IR is malformed, each naming its function. Empty when it's sound.
pub fn validate(program: &Program) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    for (at, function) in program.functions.iter().enumerate() {
        if program.functions.iter().take(at).any(|earlier| earlier.name == function.name) {
            problems.push(format!("{}: defined twice", function.name));
        }
        let mut check = Check { program, function, problems: Vec::new() };
        check.function();
        problems.extend(check.problems.into_iter().map(|problem| format!("{}: {problem}", function.name)));
    }
    if problems.is_empty() { Ok(()) } else { Err(problems) }
}

struct Check<'p> {
    program: &'p Program,
    function: &'p Function,
    problems: Vec<String>,
}

impl Check<'_> {
    fn function(&mut self) {
        let function = self.function;
        if function.params > function.locals.len() {
            self.problems.push(format!("{} parameters but fewer locals", function.params));
        }
        if function.blocks.is_empty() {
            self.problems.push("no blocks".to_owned());
        }
        for block in &function.blocks {
            for inst in &block.insts {
                self.inst(&inst.kind);
            }
            self.terminator(&block.end);
        }
    }

    fn inst(&mut self, inst: &InstKind) {
        match inst {
            InstKind::Copy { dest, value } => {
                let wanted = self.local(*dest);
                self.operand(value, wanted.as_ref(), "copy");
            }
            InstKind::Unary { dest, op, value } => {
                let wanted = self.local(*dest);
                let allowed = match op {
                    UnaryOp::Not => matches!(wanted, Some(Type::Bool)),
                    UnaryOp::Neg => matches!(wanted, Some(Type::Int | Type::Float)),
                };
                if !allowed {
                    self.problems.push(format!("`{inst}` on the wrong type"));
                }
                self.operand(value, wanted.as_ref(), "unary operator");
            }
            InstKind::Binary { dest, op, lhs, rhs } => self.binary(inst, *dest, *op, (lhs, rhs)),
            InstKind::Make { dest, type_name, fields } => self.make(*dest, type_name, fields),
            InstKind::GetField { dest, object, field } => {
                let wanted = self.local(*dest);
                self.operand(object, None, "object");
                let found = self.function.type_of(object).and_then(|ty| self.field(&ty, field));
                if found.is_none() || found != wanted {
                    self.problems.push(format!("`{inst}` reads the wrong field or type"));
                }
            }
            InstKind::SetField { object, path, value } => {
                let mut ty = self.local(*object);
                for field in path {
                    ty = ty.and_then(|ty| self.field(&ty, field));
                }
                if ty.is_none() {
                    self.problems.push(format!("`{inst}` changes a field that doesn't exist"));
                }
                self.operand(value, ty.as_ref(), "field value");
            }
            InstKind::Call { dest, function, args } => self.call(*dest, function, args),
        }
    }

    /// The type of a field of a struct type.
    fn field(&self, ty: &Type, field: &str) -> Option<Type> {
        let Type::Named(name) = ty else { return None };
        let def = self.program.structs.iter().find(|def| def.name == *name)?;
        def.fields.iter().find(|(written, _)| written == field).map(|(_, ty)| ty.clone())
    }

    fn make(&mut self, dest: LocalId, type_name: &str, fields: &[Operand]) {
        if self.local(dest) != Some(Type::Named(type_name.to_owned())) {
            self.problems.push(format!("keeps a new `{type_name}` in the wrong type"));
        }
        let Some(def) = self.program.structs.iter().find(|def| def.name == type_name) else {
            self.problems.push(format!("makes `{type_name}`, which doesn't exist"));
            return;
        };
        if fields.len() != def.fields.len() {
            self.problems.push(format!("makes `{type_name}` with {} fields", fields.len()));
        }
        for (value, (_, ty)) in fields.iter().zip(&def.fields) {
            self.operand(value, Some(ty), "field value");
        }
    }

    fn binary(&mut self, inst: &InstKind, dest: LocalId, op: BinOp, (lhs, rhs): (&Operand, &Operand)) {
        let result = self.local(dest);
        let left = self.function.type_of(lhs);
        self.operand(lhs, left.as_ref(), "left side");
        self.operand(rhs, left.as_ref(), "right side");
        let Some(left) = left else { return };
        let wanted = match op {
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                if matches!(left, Type::Int | Type::Float | Type::Bool | Type::Str) =>
            {
                Some(Type::Bool)
            }
            BinOp::Div if left == Type::Int => Some(Type::Float),
            BinOp::Add if left == Type::Str => Some(Type::Str),
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod | BinOp::Pow
                if matches!(left, Type::Int | Type::Float) =>
            {
                Some(left.clone())
            }
            _ => None,
        };
        if wanted.is_none() || wanted != result {
            self.problems.push(format!("`{inst}` on the wrong types"));
        }
    }

    fn call(&mut self, dest: Option<LocalId>, callee: &str, args: &[Operand]) {
        if let Some(dest) = dest {
            self.local(dest);
        }
        for arg in args {
            self.operand(arg, None, "argument");
        }
        let Some(target) = self.program.functions.iter().find(|function| function.name == callee) else {
            if !self.program.not_lowered.iter().any(|skipped| skipped.function == callee) {
                self.problems.push(format!("calls `{callee}`, which doesn't exist"));
            }
            return;
        };
        if args.len() != target.params {
            self.problems.push(format!("calls `{callee}` with {} arguments", args.len()));
        }
        for (arg, param) in args.iter().zip(&target.locals) {
            self.operand(arg, Some(&param.ty), "argument");
        }
        if let Some(dest) = dest
            && self.local(dest).is_some_and(|ty| ty != target.returns)
        {
            self.problems.push(format!("keeps what `{callee}` gives back in the wrong type"));
        }
    }

    fn terminator(&mut self, end: &Terminator) {
        for target in end.targets() {
            if self.function.blocks.get(target.0).is_none() {
                self.problems.push(format!("goes to {target}, which doesn't exist"));
            }
        }
        match end {
            Terminator::Branch { cond, .. } => self.operand(cond, Some(&Type::Bool), "condition"),
            Terminator::Return(Some(value)) => {
                let returns = self.function.returns.clone();
                self.operand(value, Some(&returns), "return");
            }
            Terminator::Return(None) if self.function.returns != Type::Unit => {
                self.problems.push("returns nothing, but has a return type".to_owned());
            }
            Terminator::Return(None) | Terminator::Jump(_) | Terminator::Unreachable => {}
        }
    }

    /// The type of a local this function has.
    fn local(&mut self, id: LocalId) -> Option<Type> {
        let ty = self.function.locals.get(id.0).map(|local| local.ty.clone());
        if ty.is_none() {
            self.problems.push(format!("uses {id}, which doesn't exist"));
        }
        ty
    }

    /// Checks a value exists and, when one is wanted, has that type.
    fn operand(&mut self, operand: &Operand, wanted: Option<&Type>, what: &str) {
        match self.function.type_of(operand) {
            None => self.problems.push(format!("uses {operand}, which doesn't exist")),
            Some(ty) if wanted.is_some_and(|wanted| *wanted != ty) => {
                self.problems.push(format!("{what} `{operand}` has the wrong type"));
            }
            Some(_) => {}
        }
    }
}
