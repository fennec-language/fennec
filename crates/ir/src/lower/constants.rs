//! Constants. One whose value is a literal is written out where it's read, and any other is
//! a function of no arguments that an engine runs once.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind, Item, Module, UnaryOp};

use super::{Context, Lowered, Lowering, Tail};
use crate::{Function, InstKind, NotLowered, Operand, Program};

/// A constant's value, as written. `prelude` says it's the standard library's.
#[derive(Clone, Copy)]
pub(super) struct Written<'a> {
    value: &'a Expr,
    prelude: bool,
}

/// The constants a module can read: its own, and the standard library's where it doesn't
/// have one of that name.
pub(super) fn written(module: &Module) -> HashMap<&str, Written<'_>> {
    let library = fennec_syntax::prelude_modules().unwrap_or_default();
    let mut found = HashMap::new();
    for (items, prelude) in library.iter().map(|module| (&module.items, true)).chain([(&module.items, false)])
    {
        for item in items {
            if let Item::Const { name, value, .. } = item {
                found.insert(name.as_str(), Written { value, prelude });
            }
        }
    }
    found
}

/// Lowers the module's constants and the standard library's that anything reads.
pub(super) fn lower_all(context: &Context<'_>, program: &mut Program) {
    let mut waiting: Vec<String> = context
        .constants
        .iter()
        .filter(|(_, written)| !written.prelude && literal(written.value).is_none())
        .map(|(name, _)| (*name).to_owned())
        .collect();
    waiting.sort();
    waiting.extend(read_by(&program.functions));
    while let Some(name) = waiting.pop() {
        let done = |function: &Function| function.name == name;
        if program.constants.iter().any(done)
            || program.not_lowered.iter().any(|skipped| skipped.function == name)
        {
            continue;
        }
        match Lowering::constant(&name, context) {
            Ok(mut functions) => {
                waiting.extend(read_by(&functions));
                program.functions.extend(functions.drain(1..));
                program.constants.extend(functions);
            }
            Err(why) => program.not_lowered.push(NotLowered { function: name, why }),
        }
    }
    program.constants.sort_by(|first, second| first.name.cmp(&second.name));
}

/// The names of the constants functions read.
fn read_by(functions: &[Function]) -> Vec<String> {
    let insts = functions.iter().flat_map(|function| &function.blocks).flat_map(|block| &block.insts);
    insts
        .filter_map(|inst| match &inst.kind {
            InstKind::Constant { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// A constant's value when it's a literal.
fn literal(value: &Expr) -> Option<Operand> {
    match &value.kind {
        ExprKind::Int(value) => Some(Operand::Int(*value)),
        ExprKind::Float(value) => Some(Operand::Float(*value)),
        ExprKind::Bool(value) => Some(Operand::Bool(*value)),
        ExprKind::Str(value) => Some(Operand::Str(value.clone())),
        ExprKind::Unary { op: UnaryOp::Neg, expr } => match expr.kind {
            ExprKind::Int(value) => value.checked_neg().map(Operand::Int),
            ExprKind::Float(value) => Some(Operand::Float(-value)),
            _ => None,
        },
        _ => None,
    }
}

impl<'a> Lowering<'a> {
    /// A function giving a constant's value, and those lifted out of it.
    fn constant(name: &str, context: &'a Context<'a>) -> Lowered<Vec<Function>> {
        let written = context.constants.get(name).ok_or("there's no such constant")?;
        let body = context.checked.types.body(&format!("const {name}")).ok_or("it has no types")?;
        let returns = body.name(written.value.span, name).ok_or("it has no type")?.clone();
        if !context.holds(&returns) {
            return Err(format!("it is {returns}"));
        }
        let mut lowering = Lowering::new(name, context, body, returns);
        lowering.tail_expr(written.value, Tail::Return)?;
        Ok(lowering.finished(0, Vec::new()))
    }

    /// Reads a name that isn't a local: a constant's value, when that's a literal of the type
    /// it's read as, or else the constant, or a value the engines provide, like an import of
    /// `PI`.
    pub(super) fn read_constant(&mut self, name: &str, expr: &Expr) -> Lowered<Operand> {
        if let Some(Type::Function(key)) = self.body.expr(expr.span) {
            return self.function_value(&key.clone(), expr);
        }
        let Some(written) = self.context.constants.get(name) else {
            return self.std_value(self.std_member(expr).unwrap_or_else(|| name.to_owned()), expr);
        };
        let ty = self.ty(expr)?;
        if let Some(value) = literal(written.value)
            && crate::type_in(&self.locals, &value).as_ref() == Some(&ty)
        {
            return Ok(value);
        }
        let dest = self.temporary(constant_type(self, name, written.value)?);
        self.emit(InstKind::Constant { dest, name: name.to_owned() }, expr.span);
        self.narrowed(Operand::Local(dest), expr)
    }
}

fn constant_type(lowering: &Lowering<'_>, name: &str, value: &Expr) -> Lowered<Type> {
    let body = lowering.context.checked.types.body(&format!("const {name}")).ok_or("it has no types")?;
    let ty = body.name(value.span, name).ok_or("a constant has no type")?;
    if lowering.context.holds(ty) { Ok(ty.clone()) } else { Err(format!("`{name}` is {ty}")) }
}
