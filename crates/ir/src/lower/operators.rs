//! Operators: those the engines provide, and a type's own, which are calls to its methods.

use fennec_check::Type;
use fennec_syntax::{BinOp, Span, UnaryOp};

use super::calls::owner_name;
use super::{Lowered, Lowering};
use crate::validate::{fits, plainly_ordered};
use crate::{InstKind, Operand, Terminator, type_in};

/// Why comparing values that hold a type with its own `def ==` isn't lowered yet.
pub(super) const OWN_EQUALITY: &str = "it compares values holding a type with its own `==`";

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
        let left = type_in(&self.locals, &lhs);
        if matches!(op, BinOp::Eq | BinOp::NotEq)
            && let Some(left) = left.filter(|left| self.context.signatures.own_equality(left))
        {
            return self.fields_equal(op, (lhs, rhs), &left, span);
        }
        let (lhs, rhs) =
            if matches!(op, BinOp::Eq | BinOp::NotEq) { self.one_type(lhs, rhs, span)? } else { (lhs, rhs) };
        self.check_operands(op, &lhs, &rhs)?;
        let dest = self.temporary(ty.clone());
        self.emit(InstKind::Binary { dest, op, lhs, rhs }, span);
        Ok(Operand::Local(dest))
    }

    /// `==` on a struct holding a type with its own `def ==`: each field compared in turn, with
    /// that type's `==` where it has one.
    fn fields_equal(
        &mut self,
        op: BinOp,
        (lhs, rhs): (Operand, Operand),
        ty: &Type,
        span: Span,
    ) -> Lowered<Operand> {
        let Type::Named(owner) = ty else { return Err(OWN_EQUALITY.to_owned()) };
        let fields = self.struct_fields(owner)?;
        let dest = self.temporary(Type::Bool);
        self.emit(InstKind::Copy { dest, value: Operand::Bool(true) }, span);
        let join = self.new_block();
        for (field, held) in fields {
            let (mine, theirs) = (self.temporary(held.clone()), self.temporary(held));
            self.emit(InstKind::GetField { dest: mine, object: lhs.clone(), field: field.clone() }, span);
            self.emit(InstKind::GetField { dest: theirs, object: rhs.clone(), field }, span);
            let pair = (Operand::Local(mine), Operand::Local(theirs));
            let same = self.operate(BinOp::Eq, pair, &Type::Bool, span)?;
            let (next, differ) = (self.new_block(), self.new_block());
            self.finish(Terminator::Branch { cond: same, then: next, otherwise: differ });
            self.current = differ;
            self.emit(InstKind::Copy { dest, value: Operand::Bool(false) }, span);
            self.finish(Terminator::Jump(join));
            self.current = next;
        }
        self.finish(Terminator::Jump(join));
        self.current = join;
        if op == BinOp::Eq {
            return Ok(Operand::Local(dest));
        }
        let flipped = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value: Operand::Local(dest) }, span);
        Ok(Operand::Local(flipped))
    }

    /// Makes both sides of `==` one type: a value and an optional compare as optionals.
    fn one_type(&mut self, lhs: Operand, rhs: Operand, span: Span) -> Lowered<(Operand, Operand)> {
        // A number written out beside a sized one is of its size, as the checker found.
        let sized = |operand: &Operand| matches!(type_in(&self.locals, operand), Some(Type::Sized(_)));
        let written = |operand: &Operand| matches!(operand, Operand::Int(_) | Operand::Float(_));
        if (written(&lhs) && sized(&rhs)) || (sized(&lhs) && written(&rhs)) {
            return Ok((lhs, rhs));
        }
        let left = type_in(&self.locals, &lhs).ok_or("a value has no type")?;
        let right = type_in(&self.locals, &rhs).ok_or("a value has no type")?;
        if left == right {
            return Ok((lhs, rhs));
        }
        // A member compared with its union is compared as the union: the same member, then
        // equal values.
        let member_of =
            |union: &Type, member: &Type| matches!(union, Type::Union(members) if members.contains(member));
        if member_of(&right, &left) {
            return Ok((self.as_type(lhs, &right, span)?, rhs));
        }
        if member_of(&left, &right) {
            return Ok((lhs, self.as_type(rhs, &left, span)?));
        }
        if fits(&right, &left) && !fits(&left, &right) {
            let lhs = self.as_type(lhs, &right, span)?;
            return Ok((lhs, rhs));
        }
        // A value compared with an optional of its type, as in `"kit" == found`, is compared as
        // that optional, as it is when it's on the right.
        if matches!(&right, Type::Optional(inner) if **inner == left) {
            return Ok((self.as_type(lhs, &right, span)?, rhs));
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
        let right = type_in(&self.locals, rhs);
        let given = if swap { [right, Some(left)] } else { [Some(left), right] };
        let Some((function, signature)) = self.resolve(&key, &given, None) else { return Ok(None) };
        let own = self.context.functions.contains(self.context.base_key(&key).as_str());
        let param = signature.params.get(usize::from(!swap));
        let rhs = match param {
            Some(param) if own => self.coerce(rhs.clone(), &param.ty, span)?,
            _ => rhs.clone(),
        };
        let args = if swap { vec![rhs, lhs.clone()] } else { vec![lhs.clone(), rhs] };
        let dest = self.temporary(if flip { Type::Bool } else { ty.clone() });
        self.emit(InstKind::Call { dest: Some(dest), function, args }, span);
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
        let (mut left, mut right) = (type_in(&self.locals, lhs), type_in(&self.locals, rhs));
        // A number written out beside a sized one is of its size, as the checker found.
        if matches!(lhs, Operand::Int(_) | Operand::Float(_)) && matches!(right, Some(Type::Sized(_))) {
            left.clone_from(&right);
        }
        if matches!(rhs, Operand::Int(_) | Operand::Float(_)) && matches!(left, Some(Type::Sized(_))) {
            right.clone_from(&left);
        }
        // A shift's amount can be an integer of any size.
        let shift = matches!(op, BinOp::ShiftLeft | BinOp::ShiftRight)
            && matches!(right, Some(Type::Int | Type::Sized(_)));
        let fits = left.as_ref().zip(right.as_ref()).is_some_and(|(left, right)| shift || fits(left, right))
            && match op {
                BinOp::Eq | BinOp::NotEq => true,
                BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                    left.as_ref().is_some_and(plainly_ordered)
                }
                BinOp::Add => matches!(
                    left,
                    Some(Type::Int | Type::Float | Type::Sized(_) | Type::Str | Type::Bytes | Type::List(_))
                ),
                BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod | BinOp::Pow => {
                    matches!(left, Some(Type::Int | Type::Float | Type::Sized(_)))
                }
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::ShiftLeft | BinOp::ShiftRight => {
                    matches!(left, Some(Type::Int | Type::Sized(_)))
                }
                BinOp::And | BinOp::Or | BinOp::In | BinOp::NotIn | BinOp::Is | BinOp::IsNot => false,
            };
        if fits { Ok(()) } else { Err(format!("it uses `{}` in a way that isn't lowered yet", op.symbol())) }
    }
}
