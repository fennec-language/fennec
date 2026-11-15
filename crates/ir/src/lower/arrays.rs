//! Fixed-size arrays, `array[T, N]`: what lowering does for them beyond what it does for lists.
//! An array's length is known here, so `len` is a number and a loop over one counts to it. What
//! the engines take a list for is given an array's items as one.

use fennec_check::Type;
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, Span, UnaryOp};

use super::lists::Counting;
use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, Terminator, type_in};

/// The name a loop through an array's items gives each, which no name in a program can be.
const ITEM: &str = "\0array item";

/// The engines' functions that go through what they're given, which take an array as a list of
/// its items.
const TAKE_ITEMS: &[&str] = &[
    "list",
    "set",
    "dict",
    "tuple",
    "sum",
    "min",
    "max",
    "sorted",
    "reversed",
    "any",
    "all",
    "enumerate",
    "zip",
];

impl Lowering<'_> {
    /// `array[T, N]()` as `array.new[T, N](zero)`, with the zero the checker writes into a
    /// program's own code: 0, 0.0, False, "", None, an array of zeros, or a struct made with all
    /// its defaults. A call still as written is the standard library's, which is lowered as it
    /// was checked, as its `list[T]()` and `len(x)` are.
    pub(super) fn filled_array(&self, expr: &Expr, callee: &Expr, args: &[Arg]) -> Option<Expr> {
        let ExprKind::Index { object, index } = &callee.kind else { return None };
        if !args.is_empty() || !matches!(&object.kind, ExprKind::Name(name) if name == "array") {
            return None;
        }
        let Some(Type::Array(item, _)) = self.type_at(expr.span) else { return None };
        // The item's type as written, which a zero that's an array or a struct is a call of.
        let written = match &index.kind {
            ExprKind::Tuple(parts) => parts.first()?,
            _ => index.as_ref(),
        };
        let kind = match *item {
            Type::Int => ExprKind::Int(0),
            Type::Float => ExprKind::Float(0.0),
            Type::Bool => ExprKind::Bool(false),
            Type::Str => ExprKind::Str(String::new()),
            Type::Optional(_) => ExprKind::None,
            Type::Array(..) | Type::Named(_) => {
                ExprKind::Call { callee: Box::new(written.clone()), args: Vec::new() }
            }
            _ => return None,
        };
        // The zero sits at the span of the array's type, where the checker put its type.
        let mut filled = expr.clone();
        fennec_syntax::visit::filled_array(&mut filled, &Expr { kind, span: callee.span });
        Some(filled)
    }

    /// `len(items)` or `items.len()` on an array: its size, known here.
    pub(super) fn array_len(&self, name: &str, values: &[Operand]) -> Option<i64> {
        let [items] = values else { return None };
        if !matches!(name, "len" | "array.len") {
            return None;
        }
        match type_in(&self.locals, items)? {
            Type::Array(_, size) => i64::try_from(size).ok(),
            _ => None,
        }
    }

    /// Gives a function of the engines that goes through what it's given each array's items as a
    /// new list.
    pub(super) fn arrays_as_lists(&mut self, name: &str, values: &mut [Operand], span: Span) -> Lowered<()> {
        let called = name.split('.').next().unwrap_or(name);
        if !TAKE_ITEMS.contains(&called) {
            return Ok(());
        }
        for value in values {
            if matches!(type_in(&self.locals, value), Some(Type::Array(..))) {
                *value = self.array_as_list(value.clone(), span)?;
            }
        }
        Ok(())
    }

    /// An array's items, in order, in a new list.
    pub(super) fn array_as_list(&mut self, array: Operand, span: Span) -> Lowered<Operand> {
        let Some(Type::Array(item, _)) = type_in(&self.locals, &array) else {
            return Err("an array that isn't one".to_owned());
        };
        let list = self.temporary(Type::List(item));
        self.emit(InstKind::MakeList { dest: list, items: Vec::new() }, span);
        self.each_item(array, span, |lowering, item| {
            let args = vec![Operand::Local(list), Operand::Local(item)];
            let kind = InstKind::Builtin { dest: None, name: "list.append".to_owned(), args, changes: true };
            lowering.emit(kind, span);
            Ok(())
        })?;
        Ok(Operand::Local(list))
    }

    /// `item in items`, and `not in`, on an array: each item compared in turn, as `==` compares
    /// them.
    pub(super) fn array_contains(
        &mut self,
        op: BinOp,
        item: &Expr,
        items: &Expr,
        span: Span,
    ) -> Lowered<(Operand, Operand, Operand)> {
        let Some(Type::Array(held, _)) = self.type_at(items.span) else {
            return Err("an array that isn't one".to_owned());
        };
        let item = self.expr(item)?;
        let (item, items) = self.both(item, |lowering| lowering.expr(items), span)?;
        let sought = self.coerce(item.clone(), &held, span)?;
        let found = self.temporary(Type::Bool);
        self.emit(InstKind::Copy { dest: found, value: Operand::Bool(false) }, span);
        self.each_item(items.clone(), span, |lowering, each| {
            let pair = (Operand::Local(each), sought);
            let same = lowering.operate(BinOp::Eq, pair, &Type::Bool, span)?;
            let (yes, next) = (lowering.new_block(), lowering.new_block());
            lowering.finish(Terminator::Branch { cond: same, then: yes, otherwise: next });
            lowering.current = yes;
            lowering.emit(InstKind::Copy { dest: found, value: Operand::Bool(true) }, span);
            lowering.finish(Terminator::Jump(next));
            lowering.current = next;
            Ok(())
        })?;
        if op == BinOp::In {
            return Ok((Operand::Local(found), item, items));
        }
        let flipped = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value: Operand::Local(found) }, span);
        Ok((Operand::Local(flipped), item, items))
    }

    /// Runs `body` for each item of an array, in order, given the local holding it.
    fn each_item<F>(&mut self, array: Operand, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self, LocalId) -> Lowered<()>,
    {
        let ty = type_in(&self.locals, &array).ok_or("an array has no type")?;
        let Type::Array(item, size) = &ty else { return Err("an array that isn't one".to_owned()) };
        let items = match array {
            Operand::Local(local) => local,
            other => {
                let local = self.temporary(ty.clone());
                self.emit(InstKind::Copy { dest: local, value: other }, span);
                local
            }
        };
        let limit = Operand::Int(i64::try_from(*size).map_err(|_| "an array too long to count")?);
        let names = [ITEM.to_owned()];
        let counting = Counting {
            names: &names,
            first: Operand::Int(0),
            limit,
            down: false,
            items: Some((items, *item.clone())),
            back: None,
        };
        self.counting(counting, span, |lowering| {
            let each = lowering.lookup(ITEM).ok_or("an array's item has no name")?;
            body(lowering, each)
        })
    }
}
