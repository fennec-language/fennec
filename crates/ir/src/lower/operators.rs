//! Operators: those the engines provide, and a type's own, which are calls to its methods.

use fennec_check::Type;
use fennec_syntax::{BinOp, Span, UnaryOp};

use super::calls::owner_name;
use super::{Lowered, Lowering};
use crate::validate::{fits, plainly_ordered};
use crate::{InstKind, Operand, type_in};

impl Lowering<'_> {
    /// `lhs op rhs`, giving a value of type `ty`.
    pub(super) fn operate(
        &mut self,
        op: BinOp,
        (lhs, rhs): (Operand, Operand),
        ty: &Type,
        span: Span,
    ) -> Lowered<Operand> {
        if let Some(value) = self.own_operator(op, (&lhs, &rhs), ty, span)? {
            return Ok(value);
        }
        let (lhs, rhs) =
            if matches!(op, BinOp::Eq | BinOp::NotEq) { self.one_type(lhs, rhs, span)? } else { (lhs, rhs) };
        self.check_operands(op, &lhs, &rhs)?;
        let dest = self.temporary(ty.clone());
        self.emit(InstKind::Binary { dest, op, lhs, rhs }, span);
        Ok(Operand::Local(dest))
    }

    /// Makes both sides of `==` one type: a value and an optional compare as optionals.
    fn one_type(&mut self, lhs: Operand, rhs: Operand, span: Span) -> Lowered<(Operand, Operand)> {
        let left = type_in(&self.locals, &lhs).ok_or("a value has no type")?;
        let right = type_in(&self.locals, &rhs).ok_or("a value has no type")?;
        if left == right {
            return Ok((lhs, rhs));
        }
        if fits(&right, &left) && !fits(&left, &right) {
            let lhs = self.as_type(lhs, &right, span)?;
            return Ok((lhs, rhs));
        }
        let rhs = self.as_type(rhs, &left, span)?;
        Ok((lhs, rhs))
    }

    /// A value held as exactly this type, in a temporary if it fits as it is.
    fn as_type(&mut self, value: Operand, ty: &Type, span: Span) -> Lowered<Operand> {
        let value = self.coerce(value, ty, span)?;
        if type_in(&self.locals, &value).as_ref() == Some(ty) {
            return Ok(value);
        }
        let dest = self.temporary(ty.clone());
        self.emit(InstKind::Copy { dest, value }, span);
        Ok(Operand::Local(dest))
    }

    /// A call to the left side's own method for an operator, if it has one. A type orders
    /// by its `<`, which gives the other comparisons by swapping sides and flipping.
    fn own_operator(
        &mut self,
        op: BinOp,
        (lhs, rhs): (&Operand, &Operand),
        ty: &Type,
        span: Span,
    ) -> Lowered<Option<Operand>> {
        let left = type_in(&self.locals, lhs).ok_or("a value has no type")?;
        let (method, swap, flip) = match op {
            BinOp::Gt => ("<", true, false),
            BinOp::LtEq => ("<", true, true),
            BinOp::GtEq => ("<", false, true),
            _ => match op.method_name() {
                Some(method) => (method, false, false),
                None => return Ok(None),
            },
        };
        let Some(key) = owner_name(&left).map(|owner| format!("{owner}.{method}")) else {
            return Ok(None);
        };
        let Some(signature) = self.context.checked.signatures.get(&key) else { return Ok(None) };
        let own = self.context.functions.contains(key.as_str());
        let rhs = match signature.params.get(1) {
            Some(param) if own => self.coerce(rhs.clone(), &param.ty, span)?,
            _ => rhs.clone(),
        };
        let args = if swap { vec![rhs, lhs.clone()] } else { vec![lhs.clone(), rhs] };
        let dest = self.temporary(if flip { Type::Bool } else { ty.clone() });
        self.emit(InstKind::Call { dest: Some(dest), function: key, args }, span);
        if !flip {
            return Ok(Some(Operand::Local(dest)));
        }
        let flipped = self.temporary(Type::Bool);
        let value = Operand::Local(dest);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value }, span);
        Ok(Some(Operand::Local(flipped)))
    }

    /// Whether the engines provide an operator on these values.
    pub(super) fn check_operands(&self, op: BinOp, lhs: &Operand, rhs: &Operand) -> Lowered<()> {
        let (left, right) = (type_in(&self.locals, lhs), type_in(&self.locals, rhs));
        let fits = left.as_ref().zip(right.as_ref()).is_some_and(|(left, right)| fits(left, right))
            && match op {
                BinOp::Eq | BinOp::NotEq => true,
                BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                    left.as_ref().is_some_and(plainly_ordered)
                }
                BinOp::Add => {
                    matches!(left, Some(Type::Int | Type::Float | Type::Str | Type::Bytes | Type::List(_)))
                }
                BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod | BinOp::Pow => {
                    matches!(left, Some(Type::Int | Type::Float))
                }
                BinOp::And | BinOp::Or | BinOp::In | BinOp::NotIn | BinOp::Is | BinOp::IsNot => false,
            };
        if fits { Ok(()) } else { Err(format!("it uses `{}` in a way that isn't lowered yet", op.symbol())) }
    }
}
