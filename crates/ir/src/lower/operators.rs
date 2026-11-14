//! Operators: those the engines provide, and a type's own, which are calls to its methods.

use fennec_check::Type;
use fennec_syntax::{BinOp, Span, UnaryOp};

use super::calls::owner_name;
use super::{Lowered, Lowering};
use crate::validate::{fits, plainly_ordered};
use crate::{BlockId, InstKind, LocalId, Operand, Terminator, type_in};

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
        // A `shared` value is compared by what it holds, as a type with its own `==` is by that:
        // so is anything holding either, part by part.
        if matches!(op, BinOp::Eq | BinOp::NotEq)
            && let Some(left) =
                left.filter(|left| self.context.signatures.own_equality(left) || self.holds_shared(left))
        {
            return self.parts_equal(op, (lhs, rhs), &left, span);
        }
        let (lhs, rhs) =
            if matches!(op, BinOp::Eq | BinOp::NotEq) { self.one_type(lhs, rhs, span)? } else { (lhs, rhs) };
        self.check_operands(op, &lhs, &rhs)?;
        let dest = self.temporary(ty.clone());
        self.emit(InstKind::Binary { dest, op, lhs, rhs }, span);
        Ok(Operand::Local(dest))
    }

    /// `==` or `!=` on values holding a `shared` value or a type with its own `def ==`, compared
    /// part by part: a `shared` value by what it holds, a struct by its fields, a list by its
    /// items, an optional and a tuple by what's in them. Each part is compared with
    /// [`Self::operate`], so a type's own `==` is called where it has one. A dict, a set or an
    /// enum holding one isn't lowered yet.
    fn parts_equal(
        &mut self,
        op: BinOp,
        (lhs, rhs): (Operand, Operand),
        ty: &Type,
        span: Span,
    ) -> Lowered<Operand> {
        let (first, second) = (self.in_local(lhs, ty, span), self.in_local(rhs, ty, span));
        let same = match ty {
            held if self.context.is_handle(held) => self.shared_equal((first, second), held, span)?,
            Type::Named(owner) => self.fields_equal((first, second), owner, span)?,
            Type::List(item) => self.lists_equal((first, second), item, span)?,
            Type::Optional(inner) => self.optionals_equal((first, second), inner, span)?,
            Type::Tuple(items) => self.tuples_equal((first, second), items, span)?,
            _ => return Err(OWN_EQUALITY.to_owned()),
        };
        if op == BinOp::Eq {
            return Ok(Operand::Local(same));
        }
        let flipped = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value: Operand::Local(same) }, span);
        Ok(Operand::Local(flipped))
    }

    /// A value in a local of its own, for reading parts of.
    fn in_local(&mut self, value: Operand, ty: &Type, span: Span) -> LocalId {
        match value {
            Operand::Local(local) => local,
            other => {
                let local = self.temporary(ty.clone());
                self.emit(InstKind::Copy { dest: local, value: other }, span);
                local
            }
        }
    }

    /// Sets `same` to false and goes to `out` unless `pair`'s two values are equal: the first
    /// parts that differ settle it, and no part after them is compared.
    fn and_equal(
        &mut self,
        same: LocalId,
        pair: (Operand, Operand),
        out: BlockId,
        span: Span,
    ) -> Lowered<()> {
        let equal = self.operate(BinOp::Eq, pair, &Type::Bool, span)?;
        let (next, differ) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: equal, then: next, otherwise: differ });
        self.current = differ;
        self.emit(InstKind::Copy { dest: same, value: Operand::Bool(false) }, span);
        self.finish(Terminator::Jump(out));
        self.current = next;
        Ok(())
    }

    /// Two `shared` values: what they hold, compared with both locks held to read. One compared
    /// with itself takes its lock once. Two take theirs in the order of their cells' addresses,
    /// as a `lock` of both does, so `a == b` on one thread and `b == a` on another never wait on
    /// each other. The checker refuses the comparison inside a `lock` of either, so neither lock
    /// is this thread's already.
    fn shared_equal(
        &mut self,
        (first, second): (LocalId, LocalId),
        ty: &Type,
        span: Span,
    ) -> Lowered<LocalId> {
        let Type::Named(cell) = ty else { return Err(OWN_EQUALITY.to_owned()) };
        let fields = self.struct_fields(cell)?;
        let held = fields.into_iter().find(|(name, _)| name == "value").map(|(_, held)| held);
        let held = held.ok_or("a `shared` value with nothing held")?;
        let handles = (Operand::Local(first), Operand::Local(second));
        // Which cell each is: the same one, when the two are one `shared` value.
        let one_cell = self.temporary(Type::Bool);
        let cells = InstKind::Binary {
            dest: one_cell,
            op: BinOp::Eq,
            lhs: handles.0.clone(),
            rhs: handles.1.clone(),
        };
        self.emit(cells, span);
        let same = self.temporary(Type::Bool);
        let (one, two, join) = (self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(one_cell), then: one, otherwise: two });
        self.current = one;
        self.emit(InstKind::Lock { handle: handles.0.clone(), write: false }, span);
        self.held_equal(same, &[first], (first, first), &held, span)?;
        self.emit(InstKind::Unlock { handle: handles.0.clone() }, span);
        self.finish(Terminator::Jump(join));
        self.current = two;
        let both = vec![(handles.0.clone(), false), (handles.1.clone(), false)];
        self.emit(InstKind::LockAll { handles: both }, span);
        self.held_equal(same, &[first, second], (first, second), &held, span)?;
        self.emit(InstKind::UnlockAll { handles: vec![handles.0, handles.1] }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(same)
    }

    /// Sets `same` to whether what two `shared` values hold is equal, with the locks of `locked`
    /// held: reading what one holds takes no lock of its own then.
    fn held_equal(
        &mut self,
        same: LocalId,
        locked: &[LocalId],
        (first, second): (LocalId, LocalId),
        held: &Type,
        span: Span,
    ) -> Lowered<()> {
        let before = self.locked.len();
        self.locked.extend_from_slice(locked);
        let (mine, theirs) = (self.temporary(held.clone()), self.temporary(held.clone()));
        let value = || "value".to_owned();
        self.emit(InstKind::GetField { dest: mine, object: Operand::Local(first), field: value() }, span);
        self.emit(InstKind::GetField { dest: theirs, object: Operand::Local(second), field: value() }, span);
        let equal =
            self.operate(BinOp::Eq, (Operand::Local(mine), Operand::Local(theirs)), &Type::Bool, span);
        self.locked.truncate(before);
        self.emit(InstKind::Copy { dest: same, value: equal? }, span);
        Ok(())
    }

    /// Two structs: each field compared in turn, up to the first that differs.
    fn fields_equal(
        &mut self,
        (first, second): (LocalId, LocalId),
        owner: &str,
        span: Span,
    ) -> Lowered<LocalId> {
        let fields = self.struct_fields(owner)?;
        let same = self.temporary(Type::Bool);
        self.emit(InstKind::Copy { dest: same, value: Operand::Bool(true) }, span);
        let done = self.new_block();
        for (field, held) in fields {
            let (mine, theirs) = (self.temporary(held.clone()), self.temporary(held));
            let object = Operand::Local(first);
            self.emit(InstKind::GetField { dest: mine, object, field: field.clone() }, span);
            self.emit(InstKind::GetField { dest: theirs, object: Operand::Local(second), field }, span);
            self.and_equal(same, (Operand::Local(mine), Operand::Local(theirs)), done, span)?;
        }
        self.finish(Terminator::Jump(done));
        self.current = done;
        Ok(same)
    }

    /// Two lists: as long as each other, and each item equal to the one at its place, up to the
    /// first that isn't.
    fn lists_equal(
        &mut self,
        (first, second): (LocalId, LocalId),
        item: &Type,
        span: Span,
    ) -> Lowered<LocalId> {
        let length = |lowering: &mut Self, list: LocalId| {
            let dest = lowering.temporary(Type::Int);
            let (name, args) = ("list.len".to_owned(), vec![Operand::Local(list)]);
            lowering.emit(InstKind::Builtin { dest: Some(dest), name, args, changes: false }, span);
            dest
        };
        let (mine, theirs) = (length(self, first), length(self, second));
        let same = self.temporary(Type::Bool);
        let lengths = (Operand::Local(mine), Operand::Local(theirs));
        self.emit(InstKind::Binary { dest: same, op: BinOp::Eq, lhs: lengths.0, rhs: lengths.1 }, span);
        // Items are read only from lists of one length.
        let (items, done) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(same), then: items, otherwise: done });
        self.current = items;
        // No name a program can write.
        let names = ["\0compared".to_owned()];
        let counting = super::lists::Counting {
            names: &names,
            first: Operand::Int(0),
            limit: Operand::Local(mine),
            down: false,
            items: None,
            back: None,
        };
        self.counting(counting, span, |lowering| {
            let at = lowering.lookup("\0compared").ok_or("a place in a list has no name")?;
            let (mine, theirs) = (lowering.temporary(item.clone()), lowering.temporary(item.clone()));
            let index = Operand::Local(at);
            lowering.emit(
                InstKind::GetIndex { dest: mine, object: Operand::Local(first), index: index.clone() },
                span,
            );
            lowering.emit(InstKind::GetIndex { dest: theirs, object: Operand::Local(second), index }, span);
            // The first items that differ end the loop.
            let out = lowering.loops.last().map(|within| within.exit).ok_or("a loop went missing")?;
            lowering.and_equal(same, (Operand::Local(mine), Operand::Local(theirs)), out, span)
        })?;
        self.finish(Terminator::Jump(done));
        self.current = done;
        Ok(same)
    }

    /// Two optionals: both missing, or both there and what they hold equal.
    fn optionals_equal(
        &mut self,
        (first, second): (LocalId, LocalId),
        inner: &Type,
        span: Span,
    ) -> Lowered<LocalId> {
        let missing = |lowering: &mut Self, value: LocalId| {
            let dest = lowering.temporary(Type::Bool);
            lowering.emit(InstKind::IsNone { dest, value: Operand::Local(value) }, span);
            dest
        };
        let (mine, theirs) = (missing(self, first), missing(self, second));
        let same = self.temporary(Type::Bool);
        let either = (Operand::Local(mine), Operand::Local(theirs));
        self.emit(InstKind::Binary { dest: same, op: BinOp::Eq, lhs: either.0, rhs: either.1 }, span);
        // Both there: one missing with the other there has already said no, and both missing yes.
        let (there, one_there, done) = (self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(mine), then: done, otherwise: one_there });
        self.current = one_there;
        self.finish(Terminator::Branch { cond: Operand::Local(theirs), then: done, otherwise: there });
        self.current = there;
        let (held, other) = (self.temporary(inner.clone()), self.temporary(inner.clone()));
        self.emit(InstKind::GetSome { dest: held, value: Operand::Local(first) }, span);
        self.emit(InstKind::GetSome { dest: other, value: Operand::Local(second) }, span);
        self.and_equal(same, (Operand::Local(held), Operand::Local(other)), done, span)?;
        self.finish(Terminator::Jump(done));
        self.current = done;
        Ok(same)
    }

    /// Two tuples: each item compared in turn, up to the first that differs.
    fn tuples_equal(
        &mut self,
        (first, second): (LocalId, LocalId),
        items: &[Type],
        span: Span,
    ) -> Lowered<LocalId> {
        let same = self.temporary(Type::Bool);
        self.emit(InstKind::Copy { dest: same, value: Operand::Bool(true) }, span);
        let done = self.new_block();
        for (at, item) in items.iter().enumerate() {
            let (mine, theirs) = (self.temporary(item.clone()), self.temporary(item.clone()));
            self.emit(InstKind::GetTupleItem { dest: mine, value: Operand::Local(first), at }, span);
            self.emit(InstKind::GetTupleItem { dest: theirs, value: Operand::Local(second), at }, span);
            self.and_equal(same, (Operand::Local(mine), Operand::Local(theirs)), done, span)?;
        }
        self.finish(Terminator::Jump(done));
        self.current = done;
        Ok(same)
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
        // A type with the operator again for another type on its right calls the one for this.
        let key = match (&right, swap) {
            (Some(right), false) => self
                .context
                .signatures
                .operator_again(&key)
                .find(|(_, signature)| signature.params.get(1).is_some_and(|param| param.ty == *right))
                .map_or(key, |(found, _)| found.to_owned()),
            _ => key,
        };
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
