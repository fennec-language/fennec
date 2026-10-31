//! Lists and strs: writing lists out, reading and changing items, `in`, f-strings, and `for`
//! loops, which count in a local of their own.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{BinOp, Expr, ExprKind, FStrPart, Span, Stmt, UnaryOp};

use super::{Loop, Lowered, Lowering, Tail};
use crate::{FormatPart, InstKind, LocalId, Operand, Terminator};

/// What a `for` loop counts through. Its name holds the count, or the item at the count.
struct Counting<'a> {
    name: &'a str,
    first: Operand,
    limit: Operand,
    down: bool,
    items: Option<(LocalId, Type)>,
}

impl Lowering<'_> {
    pub(super) fn list(&mut self, expr: &Expr, items: &[Expr]) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let Type::List(item) = &ty else { return Err("a list has another type".to_owned()) };
        let mut values = Vec::with_capacity(items.len());
        for written in items {
            let value = self.expr(written)?;
            values.push(self.coerce(value, item, written.span)?);
        }
        let dest = self.temporary(ty);
        self.emit(InstKind::MakeList { dest, items: values }, expr.span);
        Ok(Operand::Local(dest))
    }

    pub(super) fn get_index(&mut self, expr: &Expr, object: &Expr, index: &Expr) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let readable = match self.body.expr(object.span) {
            Some(Type::List(item)) => **item == ty,
            Some(Type::Str) => ty == Type::Str,
            _ => false,
        };
        if !readable || self.body.expr(index.span) != Some(&Type::Int) {
            return Err("it uses indexing on something other than a list or str".to_owned());
        }
        let object = self.expr(object)?;
        let index = self.expr(index)?;
        let dest = self.temporary(ty);
        self.emit(InstKind::GetIndex { dest, object, index }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// `object.path[index] = value`, and `+=` and so on.
    pub(super) fn assign_index(
        &mut self,
        (object, index): (&Expr, &Expr),
        op: Option<BinOp>,
        value: &Expr,
        span: Span,
    ) -> Lowered<()> {
        let (local, path) = self.field_place(object)?;
        let Some(Type::List(item)) = self.place_type(local, &path) else {
            return Err("it changes an item of something other than a list".to_owned());
        };
        if self.body.expr(index.span) != Some(&Type::Int) {
            return Err("it changes an item by something other than an int".to_owned());
        }
        let index = self.expr(index)?;
        let mut value = self.expr(value)?;
        if let Some(op) = op {
            let list = self.read_place(local, &path, span)?;
            let read = self.temporary(*item.clone());
            let get = InstKind::GetIndex { dest: read, object: list, index: index.clone() };
            self.emit(get, span);
            self.check_operands(op, &Operand::Local(read), &value)?;
            let dest = self.temporary(*item.clone());
            self.emit(InstKind::Binary { dest, op, lhs: Operand::Local(read), rhs: value }, span);
            value = Operand::Local(dest);
        }
        let value = self.coerce(value, &item, span)?;
        self.emit(InstKind::SetIndex { object: local, path, index, value }, span);
        Ok(())
    }

    /// `item in items`, and `not in`, on a list or str.
    pub(super) fn contains(&mut self, op: BinOp, item: &Expr, items: &Expr, span: Span) -> Lowered<Operand> {
        let owner = match self.body.expr(items.span) {
            Some(Type::List(_)) => "list",
            Some(Type::Str) => "str",
            _ => return Err(format!("it uses `{}` on something other than a list or str", op.symbol())),
        };
        let item = self.expr(item)?;
        let items = self.expr(items)?;
        let dest = self.temporary(Type::Bool);
        let name = format!("{owner}.contains");
        self.emit(
            InstKind::Builtin { dest: Some(dest), name, args: vec![items, item], changes: false },
            span,
        );
        if op == BinOp::In {
            return Ok(Operand::Local(dest));
        }
        let flipped = self.temporary(Type::Bool);
        let value = Operand::Local(dest);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value }, span);
        Ok(Operand::Local(flipped))
    }

    pub(super) fn format(&mut self, parts: &[FStrPart], span: Span) -> Lowered<Operand> {
        let mut lowered = Vec::with_capacity(parts.len());
        for part in parts {
            lowered.push(match part {
                FStrPart::Text(text) => FormatPart::Text(text.clone()),
                FStrPart::Expr { expr, spec } => {
                    FormatPart::Value { value: self.expr(expr)?, spec: spec.clone() }
                }
            });
        }
        let dest = self.temporary(Type::Str);
        self.emit(InstKind::Format { dest, parts: lowered }, span);
        Ok(Operand::Local(dest))
    }

    /// A `for` loop over a `range`, a list or a str.
    pub(super) fn for_stmt(
        &mut self,
        (names, is_mut): (&[String], bool),
        iter: &Expr,
        body: &[Stmt],
        span: Span,
    ) -> Lowered<()> {
        let [name] = names else { return Err("it has a `for` loop that unpacks".to_owned()) };
        if is_mut {
            return Err("it has a `for mut` loop".to_owned());
        }
        if self.is_range(iter) {
            let step = "it has a `for` loop over a `range` stepping by something other than 1 or -1";
            let (args, down) = range_args(iter).ok_or(step)?;
            let (first, limit) = match args.as_slice() {
                [limit] => (Operand::Int(0), self.expr(limit)?),
                [first, limit] => (self.expr(first)?, self.expr(limit)?),
                _ => return Err(step.to_owned()),
            };
            return self.counting(Counting { name, first, limit, down, items: None }, body, span);
        }
        let ty = self.body.expr(iter.span).cloned().ok_or("what it loops over has no type")?;
        let value = self.expr(iter)?;
        let (items, item) = match &ty {
            // A loop goes through the list as it was, whatever the body does to the name.
            Type::List(item) if self.context.holds(item) => {
                let held = self.temporary(ty.clone());
                self.emit(InstKind::Copy { dest: held, value }, span);
                (held, *item.clone())
            }
            Type::Str => {
                let held = self.temporary(Type::List(Box::new(Type::Str)));
                let args = vec![value];
                let kind = InstKind::Builtin {
                    dest: Some(held),
                    name: "str.chars".to_owned(),
                    args,
                    changes: false,
                };
                self.emit(kind, span);
                (held, Type::Str)
            }
            other => return Err(format!("it has a `for` loop over {other}")),
        };
        let limit = self.temporary(Type::Int);
        let args = vec![Operand::Local(items)];
        let kind = InstKind::Builtin { dest: Some(limit), name: "list.len".to_owned(), args, changes: false };
        self.emit(kind, span);
        let counting = Counting {
            name,
            first: Operand::Int(0),
            limit: Operand::Local(limit),
            down: false,
            items: Some((items, item)),
        };
        self.counting(counting, body, span)
    }

    /// Counts in a local of its own, so the loop's name is a fresh value each time around.
    /// The limit is read once. Going one step at a time, the count can't overflow, since
    /// it's short of the limit before each step.
    fn counting(&mut self, loop_of: Counting<'_>, body: &[Stmt], span: Span) -> Lowered<()> {
        let Counting { name, first, limit, down, items } = loop_of;
        let count = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: count, value: first }, span);
        let stop = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: stop, value: limit }, span);
        let (head, then, next, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Jump(head));
        self.current = head;
        let going = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(count), Operand::Local(stop));
        let op = if down { BinOp::Gt } else { BinOp::Lt };
        self.emit(InstKind::Binary { dest: going, op, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(going), then, otherwise: exit });
        self.current = then;
        self.scopes.push(HashMap::new());
        if let Some((items, ty)) = items {
            let dest = self.declare(name, ty);
            let (object, index) = (Operand::Local(items), Operand::Local(count));
            self.emit(InstKind::GetIndex { dest, object, index }, span);
        } else {
            let dest = self.declare(name, Type::Int);
            self.emit(InstKind::Copy { dest, value: Operand::Local(count) }, span);
        }
        self.loops.push(Loop { next, exit });
        let lowered = self.stmts(body, Tail::Drop);
        self.loops.pop();
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        let op = if down { BinOp::Sub } else { BinOp::Add };
        let lhs = Operand::Local(count);
        self.emit(InstKind::Binary { dest: count, op, lhs, rhs: Operand::Int(1) }, span);
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }

    /// Whether an expression is a call to the built-in `range`.
    fn is_range(&self, iter: &Expr) -> bool {
        let ExprKind::Call { callee, .. } = &iter.kind else { return false };
        matches!(&callee.kind, ExprKind::Name(name) if name == "range" && self.lookup(name).is_none())
            && !self.context.functions.contains("range")
    }

    /// The type a local's field holds, following a path.
    pub(super) fn place_type(&self, local: LocalId, path: &[String]) -> Option<Type> {
        let mut ty = self.locals.get(local.0).map(|held| held.ty.clone());
        for field in path {
            let Some(Type::Named(owner)) = &ty else { return None };
            let found = self.context.checked.signatures.structure(owner)?;
            ty = found.fields.iter().find(|(name, _)| name == field).map(|(_, ty)| ty.clone());
        }
        ty
    }

    /// Reads a local's field into a temporary, following a path.
    pub(super) fn read_place(&mut self, local: LocalId, path: &[String], span: Span) -> Lowered<Operand> {
        let mut read = Operand::Local(local);
        for (at, field) in path.iter().enumerate() {
            let ty = self.place_type(local, path.get(..=at).unwrap_or_default());
            let ty = ty.filter(|ty| self.context.holds(ty)).ok_or_else(|| format!("it reads `.{field}`"))?;
            let dest = self.temporary(ty);
            self.emit(InstKind::GetField { dest, object: read, field: field.clone() }, span);
            read = Operand::Local(dest);
        }
        Ok(read)
    }
}

/// The first and limit arguments of a call to `range` with a step of 1 or -1, and
/// whether it counts down.
fn range_args(iter: &Expr) -> Option<(Vec<&Expr>, bool)> {
    let ExprKind::Call { args, .. } = &iter.kind else { return None };
    if args.iter().any(|arg| arg.name.is_some() || arg.is_mut) {
        return None;
    }
    let values: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
    match values.as_slice() {
        [first, limit, step] => {
            let down = match &step.kind {
                ExprKind::Int(1) => false,
                ExprKind::Int(-1) => true,
                ExprKind::Unary { op: UnaryOp::Neg, expr } if matches!(expr.kind, ExprKind::Int(1)) => true,
                _ => return None,
            };
            Some((vec![first, limit], down))
        }
        _ => Some((values, false)),
    }
}
