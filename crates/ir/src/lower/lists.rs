//! Lists and strs: writing lists out, reading and changing items, `in`, f-strings, and `for`
//! loops, which count in a local of their own.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{BinOp, CompKind, Expr, ExprKind, FStrPart, Span, Stmt, UnaryOp};

use super::calls::owner_name;
use super::generics::FunctionInstance;
use super::places::{Leaving, Place};
use super::{Loop, Lowered, Lowering, Tail};
use crate::{BlockId, FormatPart, InstKind, LocalId, Operand, Terminator};

/// What a `for` loop counts through. Its names hold the count, or the item at the count,
/// unpacked when there are several.
pub(super) struct Counting<'a> {
    pub(super) names: &'a [String],
    pub(super) first: Operand,
    pub(super) limit: Operand,
    pub(super) down: bool,
    pub(super) items: Option<(LocalId, Type)>,
    /// Where each item goes back to, for a `for mut` loop.
    pub(super) back: Option<Back>,
}

/// Where a `for mut` loop's items go back to.
pub(super) struct Back {
    place: Place,
    /// It goes through `Table` items by their `Id`s, not by position.
    by_id: bool,
}

/// A `Table` a loop goes through by its slots: read once, or from its place each time it's asked.
struct Walk<'a> {
    iter: &'a Expr,
    once: Option<Operand>,
    span: Span,
}

impl Walk<'_> {
    /// Asks the `Table` something of itself, or of the slot `count` is at, into `dest`.
    fn ask(
        &self,
        lowering: &mut Lowering<'_>,
        dest: LocalId,
        name: &str,
        count: Option<LocalId>,
    ) -> Lowered<()> {
        let table = match &self.once {
            Some(value) => value.clone(),
            None => lowering.expr(self.iter)?,
        };
        let mut args = vec![table];
        args.extend(count.map(Operand::Local));
        let kind = InstKind::Builtin { dest: Some(dest), name: name.to_owned(), args, changes: false };
        lowering.emit(kind, self.span);
        Ok(())
    }
}

/// What a loop goes through.
pub(super) struct Over<'a> {
    pub(super) names: &'a [String],
    pub(super) iter: &'a Expr,
    pub(super) back: Option<Back>,
}

impl Lowering<'_> {
    /// `list.pop()` on a place that can be read twice, like `self.items`: the last item, or
    /// `None` when it's empty. The length is tested first, and only then is the list taken
    /// out, popped and put back, with nothing between, so it has one owner while it changes and
    /// isn't copied.
    pub(super) fn pop_or_none(
        &mut self,
        callee: &Expr,
        object: &Expr,
        keep: Option<&Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        let list = self.expr(object)?;
        let dest = keep.map(|ty| self.temporary(ty.clone()));
        let some = self.when_not_empty(list, dest, span);
        self.popping = true;
        let item = match keep {
            Some(Type::Optional(item)) => Some((**item).clone()),
            other => other.cloned(),
        };
        let popped = self.call(callee, &[], item, span);
        self.popping = false;
        self.popped_into(dest, popped?, keep, some, span)
    }

    /// The same for a place that can't be read twice, tested and popped where it is. A list in
    /// a field may be copied here, since the test comes between taking it out and putting it back.
    pub(super) fn pop_in_block(
        &mut self,
        values: Vec<Operand>,
        keep: Option<&Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        let list = values.first().cloned().ok_or("`pop` with nothing to pop")?;
        let Some(Type::List(item)) = crate::type_in(&self.locals, &list) else {
            return Err("`pop` on something that isn't a list".to_owned());
        };
        let dest = keep.map(|ty| self.temporary(ty.clone()));
        let some = self.when_not_empty(list, dest, span);
        let popped = dest.map(|_| self.temporary(*item));
        let name = "list.pop".to_owned();
        self.emit(InstKind::Builtin { dest: popped, name, args: values, changes: true }, span);
        self.popped_into(dest, popped, keep, some, span)
    }

    /// Tests whether `list` is empty, giving `dest` `None` if it is, and carries on in the block
    /// where it isn't. It gives back the block that the two meet in.
    fn when_not_empty(&mut self, list: Operand, dest: Option<LocalId>, span: Span) -> BlockId {
        let length = self.temporary(Type::Int);
        let name = "list.len".to_owned();
        self.emit(InstKind::Builtin { dest: Some(length), name, args: vec![list], changes: false }, span);
        let empty = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(length), Operand::Int(0));
        self.emit(InstKind::Binary { dest: empty, op: BinOp::Eq, lhs, rhs }, span);
        let (none, some, join) = (self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(empty), then: none, otherwise: some });
        self.current = none;
        if let Some(dest) = dest {
            self.emit(InstKind::Copy { dest, value: Operand::None }, span);
        }
        self.finish(Terminator::Jump(join));
        self.current = some;
        join
    }

    /// Puts what was popped in `dest` as an optional, and goes on where the empty case meets it.
    fn popped_into(
        &mut self,
        dest: Option<LocalId>,
        popped: Option<LocalId>,
        keep: Option<&Type>,
        join: BlockId,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        if let (Some(dest), Some(popped), Some(ty)) = (dest, popped, keep) {
            let value = self.coerce(Operand::Local(popped), ty, span)?;
            self.emit(InstKind::Copy { dest, value }, span);
        }
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(dest)
    }

    pub(super) fn list(&mut self, expr: &Expr, items: &[Expr]) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let (Type::List(item) | Type::Array(item, _)) = &ty else {
            return Err("a list has another type".to_owned());
        };
        let values = self.items_as(items, item, expr.span)?;
        let dest = self.temporary(ty);
        self.emit(InstKind::MakeList { dest, items: values }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// What `embed_folder` gave: its paths and its files, each a list of constants, in an
    /// `EmbeddedFolder`.
    pub(super) fn folder(&mut self, files: &[(String, fennec_syntax::Blob)], span: Span) -> Operand {
        let paths = self.temporary(Type::List(Box::new(Type::Str)));
        let items = files.iter().map(|(path, _)| Operand::Str(path.clone())).collect();
        self.emit(InstKind::ConstantList { dest: paths, items }, span);
        let data = self.temporary(Type::List(Box::new(Type::Bytes)));
        let items = files.iter().map(|(_, data)| Operand::Bytes(data.clone())).collect();
        self.emit(InstKind::ConstantList { dest: data, items }, span);
        let dest = self.temporary(Type::Named("EmbeddedFolder".to_owned()));
        let fields = vec![Operand::Local(paths), Operand::Local(data)];
        self.emit(InstKind::Make { dest, type_name: "EmbeddedFolder".to_owned(), fields }, span);
        Operand::Local(dest)
    }

    /// `object[index]`. A tuple's item at a position written as a number is known here.
    pub(super) fn get_index(&mut self, expr: &Expr, object: &Expr, index: &Expr) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let held = self.type_at(object.span);
        if let (Some(Type::Tuple(items)), ExprKind::Int(at)) = (&held, &index.kind)
            && let Some(at) = usize::try_from(*at).ok().filter(|at| items.get(*at) == Some(&ty))
        {
            let value = self.expr(object)?;
            let dest = self.temporary(ty);
            self.emit(InstKind::GetTupleItem { dest, value, at }, expr.span);
            return Ok(Operand::Local(dest));
        }
        let key = match &held {
            Some(
                Type::List(_) | Type::Array(..) | Type::Buffer(_) | Type::Str | Type::Bytes | Type::Tuple(_),
            ) => Type::Int,
            Some(Type::Dict(key, _)) => *key.clone(),
            Some(Type::Table(item)) => Type::Id(item.clone()),
            Some(Type::Named(name)) if name == "Args" => {
                let object = self.expr(object)?;
                let index = self.expr(index)?;
                let dest = self.temporary(ty);
                self.emit(InstKind::GetIndex { dest, object, index }, expr.span);
                return Ok(Operand::Local(dest));
            }
            Some(Type::Named(owner)) => {
                let found = self.resolve(&format!("{owner}.[]"), std::slice::from_ref(&held), Some(&ty));
                if found.is_none() && self.has_mut_form(owner) {
                    let place = self.place_of(expr)?;
                    return self.read_steps(place.local, &place.steps, expr.span);
                }
                let param = found.as_ref().and_then(|(_, found)| found.params.get(1));
                param.ok_or_else(|| format!("it indexes `{owner}`, which has no `[]`"))?.ty.clone()
            }
            _ => return Err("it uses indexing on something the IR doesn't index".to_owned()),
        };
        let object = self.expr(object)?;
        let (object, index) = self.both(
            object,
            |lowering| {
                let lowered = lowering.expr(index)?;
                lowering.coerce(lowered, &key, index.span)
            },
            expr.span,
        )?;
        Ok(Operand::Local(self.read_item(held.as_ref().unwrap_or(&ty), object, index, &ty, expr.span)))
    }

    /// `item in value` when `value`'s type has its own `def in`: a call to it, with the value
    /// first, and `not in` its opposite. The item is worked out first, as it's written first.
    fn own_in(
        &mut self,
        op: BinOp,
        item: &Expr,
        items: &Expr,
        span: Span,
    ) -> Lowered<Option<(Operand, Operand, Operand)>> {
        let Some(held) = self.type_at(items.span) else { return Ok(None) };
        let Some(key) = owner_name(&held).map(|owner| format!("{owner}.in")) else { return Ok(None) };
        let given = [Some(held.clone()), self.type_at(item.span)];
        let Some((function, signature)) = self.resolve(&key, &given, None) else { return Ok(None) };
        let item = self.expr(item)?;
        let (item, items) = self.both(item, |lowering| lowering.expr(items), span)?;
        let own = self.context.functions.contains(self.context.base_key(&key).as_str());
        let item = match signature.params.get(1) {
            Some(param) if own => self.coerce(item, &param.ty, span)?,
            _ => item,
        };
        let dest = self.temporary(Type::Bool);
        let args = vec![items.clone(), item.clone()];
        self.emit(InstKind::Call { dest: Some(dest), function, args }, span);
        if op == BinOp::In {
            return Ok(Some((Operand::Local(dest), item, items)));
        }
        let flipped = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value: Operand::Local(dest) }, span);
        Ok(Some((Operand::Local(flipped), item, items)))
    }

    /// `item in items`, and `not in`, on a collection.
    pub(super) fn contains(&mut self, op: BinOp, item: &Expr, items: &Expr, span: Span) -> Lowered<Operand> {
        self.contains_of(op, item, items, span).map(|(test, _, _)| test)
    }

    /// The same, with both sides.
    pub(super) fn contains_of(
        &mut self,
        op: BinOp,
        item: &Expr,
        items: &Expr,
        span: Span,
    ) -> Lowered<(Operand, Operand, Operand)> {
        if let Some(found) = self.own_in(op, item, items, span)? {
            return Ok(found);
        }
        if let Some(Type::Array(..)) = self.type_at(items.span) {
            return self.array_contains(op, item, items, span);
        }
        let owner = match self.type_at(items.span) {
            Some(
                ref ty @ (Type::List(_)
                | Type::Set(_)
                | Type::Dict(..)
                | Type::Table(_)
                | Type::Str
                | Type::Bytes),
            ) => owner_name(ty).unwrap_or_default().to_owned(),
            _ => return Err(format!("it uses `{}` on a type of its own", op.symbol())),
        };
        // A list of `shared` values, or of a type with its own `==`, is asked item by item, each
        // compared as `==` compares it.
        if let Some(Type::List(held)) = self.type_at(items.span).and_then(|ty| self.held(&ty))
            && (self.context.signatures.own_equality(&held) || self.holds_shared(&held))
        {
            let sought = self.expr(item)?;
            let sought = self.coerce(sought, &held, item.span)?;
            let (sought, list) = self.both(sought, |lowering| lowering.expr(items), span)?;
            let found = self.list_holds(list.clone(), sought.clone(), &held, span)?;
            if op == BinOp::In {
                return Ok((Operand::Local(found), sought, list));
            }
            let flipped = self.temporary(Type::Bool);
            let value = Operand::Local(found);
            self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value }, span);
            return Ok((Operand::Local(flipped), sought, list));
        }
        let mut item_at = self.expr(item)?;
        // A member sought among a union's values is made one of them.
        if let Some(Type::List(held) | Type::Set(held) | Type::Dict(held, _)) = self.type_at(items.span)
            && let Type::Union(_) = *held
        {
            item_at = self.coerce(item_at, &held, item.span)?;
        }
        let (item, items) = self.both(item_at, |lowering| lowering.expr(items), span)?;
        let dest = self.temporary(Type::Bool);
        let name = format!("{owner}.contains");
        let args = vec![items.clone(), item.clone()];
        self.emit(InstKind::Builtin { dest: Some(dest), name, args, changes: false }, span);
        if op == BinOp::In {
            return Ok((Operand::Local(dest), item, items));
        }
        let flipped = self.temporary(Type::Bool);
        let value = Operand::Local(dest);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value }, span);
        Ok((Operand::Local(flipped), item, items))
    }

    /// A value as printing shows it: when its type writes its own text with `def str`, that
    /// text, from a `StrWriter` the method fills. Any other value as it is, for printing to
    /// show as it always has.
    pub(super) fn shown(&mut self, expr: &Expr) -> Lowered<Operand> {
        let Some(held) = self.type_at(expr.span) else { return self.expr(expr) };
        let writer = Type::Named("StrWriter".to_owned());
        let Some(function) = self.own_str(&held) else {
            self.own_texts_within(&held, &mut Vec::new(), expr.span)?;
            return self.expr(expr);
        };
        let value = self.expr(expr)?;
        self.own_text_of(value, function, (writer, expr.span))
    }

    /// The text a value's own `def str` writes.
    fn own_text_of(
        &mut self,
        value: Operand,
        function: String,
        (writer, span): (Type, Span),
    ) -> Lowered<Operand> {
        let out = self.make("StrWriter", &[], span)?;
        self.emit(InstKind::Call { dest: None, function, args: vec![value, out.clone()] }, span);
        let (text, _) = self
            .resolve("StrWriter.text", &[Some(writer)], None)
            .ok_or("it prints a value with its own `def str`, but `StrWriter.text` isn't there")?;
        let dest = self.temporary(Type::Str);
        self.emit(InstKind::Call { dest: Some(dest), function: text, args: vec![out] }, span);
        Ok(Operand::Local(dest))
    }

    /// Whether a value of this type is a `Secret` or has one in it. `seen` are the types looked at.
    pub(super) fn holds_secret(&self, held: &Type, seen: &mut Vec<Type>) -> bool {
        self.holds(held, seen, &|_, ty| ty.to_string().starts_with("Secret["))
    }

    /// Whether a value of this type is a `shared` value or has one in it.
    pub(super) fn holds_shared(&self, held: &Type) -> bool {
        self.holds(held, &mut Vec::new(), &|lowering, ty| lowering.context.is_handle(ty))
    }

    /// Whether a value of this type is one `wanted` picks, or has one in it, however deep. `seen`
    /// are the types looked at.
    fn holds(&self, held: &Type, seen: &mut Vec<Type>, wanted: &dyn Fn(&Self, &Type) -> bool) -> bool {
        match held {
            Type::List(item) | Type::Set(item) | Type::Optional(item) | Type::Table(item) => {
                return self.holds(item, seen, wanted);
            }
            Type::Dict(first, second) | Type::Result(first, second) => {
                return self.holds(first, seen, wanted) || self.holds(second, seen, wanted);
            }
            Type::Tuple(items) => return items.iter().any(|item| self.holds(item, seen, wanted)),
            Type::Named(_) | Type::Generic(..) => {}
            _ => return false,
        }
        let held = self.concrete(held);
        if seen.contains(&held) {
            return false;
        }
        seen.push(held.clone());
        let name = held.to_string();
        if wanted(self, &held) {
            return true;
        }
        let mut within: Vec<Type> = self
            .struct_fields(&name)
            .map(|fields| fields.into_iter().map(|(_, ty)| ty).collect())
            .unwrap_or_default();
        if let Some(found) = self.context.enums.get(name.as_str()) {
            within.extend(found.cases.iter().flat_map(|case| case.fields.iter().map(|(_, ty)| ty.clone())));
        }
        within.iter().any(|ty| self.holds(ty, seen, wanted))
    }

    /// Has a function made for each type that writes its own text within a value of type `held`,
    /// for the runtime to call as it writes the value. `seen` are the types looked at already.
    pub(super) fn own_texts_within(&mut self, held: &Type, seen: &mut Vec<Type>, span: Span) -> Lowered<()> {
        match held {
            Type::List(item) | Type::Set(item) | Type::Optional(item) | Type::Table(item) => {
                return self.own_texts_within(item, seen, span);
            }
            Type::Dict(first, second) | Type::Result(first, second) => {
                self.own_texts_within(first, seen, span)?;
                return self.own_texts_within(second, seen, span);
            }
            Type::Tuple(items) => {
                return items.iter().try_for_each(|item| self.own_texts_within(item, seen, span));
            }
            Type::Named(_) | Type::Generic(..) => {}
            _ => return Ok(()),
        }
        // An enum's field as it's declared is `Secret[str]`, not yet the copy the program uses.
        let held = &self.concrete(held);
        if seen.contains(held) {
            return Ok(());
        }
        seen.push(held.clone());
        if let Some(function) = self.own_str(held) {
            return self.own_text_function(held, function, span);
        }
        let name = held.to_string();
        let mut within: Vec<Type> = self
            .struct_fields(&name)
            .map(|fields| fields.into_iter().map(|(_, ty)| ty).collect())
            .unwrap_or_default();
        if let Some(found) = self.context.enums.get(name.as_str()) {
            within.extend(found.cases.iter().flat_map(|case| case.fields.iter().map(|(_, ty)| ty.clone())));
        }
        within.iter().try_for_each(|ty| self.own_texts_within(ty, seen, span))
    }

    /// Makes the function giving the text a value of type `held` writes, if it isn't made yet.
    fn own_text_function(&mut self, held: &Type, function: String, span: Span) -> Lowered<()> {
        let key = format!("{held}.own_text");
        if self.context.own_texts.borrow().iter().any(|made| made.function == key) {
            return Ok(());
        }
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut made = Lowering::new(&instance, self.context, self.body, Type::Str);
        let value = made.declare("value", held.clone());
        made.scopes.push(HashMap::new());
        let writer = Type::Named("StrWriter".to_owned());
        let text = made.own_text_of(Operand::Local(value), function, (writer, span))?;
        made.finish(Terminator::Return(Some(text)));
        self.lifted.extend(made.finished(1, Vec::new()));
        self.context.own_texts.borrow_mut().push(crate::OwnText { ty: held.clone(), function: key });
        Ok(())
    }

    /// The `def str` a type writes its own text with, if it has one.
    fn own_str(&self, held: &Type) -> Option<String> {
        let key = format!("{}.str", owner_name(held)?);
        let writer = Type::Named("StrWriter".to_owned());
        self.resolve(&key, &[Some(held.clone()), Some(writer)], None).map(|(function, _)| function)
    }

    pub(super) fn format(&mut self, parts: &[FStrPart], span: Span) -> Lowered<Operand> {
        let mut reads = super::order::Reads::default();
        for part in parts {
            if let FStrPart::Expr { expr, spec } = part {
                let value = if spec.is_none() { self.shown(expr)? } else { self.expr(expr)? };
                reads.add(self, value);
            }
        }
        let mut values = reads.finish(self, span).into_iter();
        let mut lowered = Vec::with_capacity(parts.len());
        for part in parts {
            lowered.push(match part {
                FStrPart::Text(text) => FormatPart::Text(text.clone()),
                FStrPart::Expr { spec, .. } => {
                    let value = values.next().ok_or("a part of a string went missing")?;
                    FormatPart::Value { value, spec: spec.clone() }
                }
            });
        }
        let dest = self.temporary(Type::Str);
        self.emit(InstKind::Format { dest, parts: lowered }, span);
        Ok(Operand::Local(dest))
    }

    /// A `for` loop.
    pub(super) fn for_stmt(
        &mut self,
        (names, is_mut): (&[String], bool),
        iter: &Expr,
        body: &[Stmt],
        span: Span,
    ) -> Lowered<()> {
        // A `for mut` over `parallel(xs)` changes the items of `xs` where they are, and running
        // its iterations in order is a correct way to run it. So is going through a server's
        // connections in order, when they aren't each given a task.
        let in_order =
            |items: &Expr| is_mut || self.type_at(items.span) == Some(Type::Named("Server".to_owned()));
        let iter = match super::parallel::parallel_items(iter) {
            Some(items) if in_order(items) => items,
            _ => iter,
        };
        if !is_mut && let Some(expansion) = self.expanded(names, iter, body) {
            return self.fused(expansion);
        }
        if let (true, [name], Some(Type::Named(owner))) = (is_mut, names, self.type_at(iter.span))
            && self.has_mut_form(&owner)
        {
            return self.through_positions(name, (iter, &owner), body, span);
        }
        // Only the items of a list or `Table` are written back, and only one name holds them.
        let by_id = matches!(self.type_at(iter.span), Some(Type::Table(_)));
        let changes_items = is_mut
            && names.len() == 1
            && (by_id || matches!(self.type_at(iter.span), Some(Type::List(_) | Type::Array(..))));
        let back =
            if changes_items { Some(Back { place: self.held_place(iter, span)?, by_id }) } else { None };
        let over = Over { names, iter, back };
        self.looping(over, span, |lowering| lowering.stmts(body, Tail::Drop))
    }

    /// `xs = [f(x) for x in xs]`, as `for mut x in xs: x = f(x)`: each item is replaced where it
    /// is, so no second list is made unless something else holds this one. The same for a field,
    /// `self.xs = [f(x) for x in self.xs]`. Nothing can see the difference if what's worked out
    /// doesn't read the list, can't leave part of the way along, and no `where` rule looks at
    /// the list before it's finished. A list in a `shared` value is the same: its lock is held, so
    /// anything reaching the cell another way ends the program.
    pub(super) fn replaces_its_source(&mut self, target: &Expr, value: &Expr, span: Span) -> Lowered<bool> {
        let ExprKind::Comprehension { kind: CompKind::List, elem, value: None, names, iter, cond: None } =
            &value.kind
        else {
            return Ok(false);
        };
        let Some((root, path)) = field_path_of(target) else { return Ok(false) };
        let [item] = names.as_slice() else { return Ok(false) };
        let Some(Type::List(held)) = self.type_at(iter.span) else { return Ok(false) };
        if field_path_of(iter).as_ref() != Some(&(root, path.clone()))
            || item == root
            || self.type_at(value.span) != Some(Type::List(held.clone()))
            || !reads_apart(elem, root, path.first().copied())
            || self.rules_along(root, &path)
        {
            return Ok(false);
        }
        let place = self.held_place(iter, span)?;
        if self.steps_type(&place)? != Type::List(held.clone()) {
            return Ok(false);
        }
        let back = Some(Back { place, by_id: false });
        let over = Over { names, iter, back };
        self.looping(over, span, |lowering| {
            let made = lowering.expr(elem)?;
            let made = lowering.coerce(made, &held, span)?;
            let dest = lowering.lookup(item).ok_or("a loop's item has no name")?;
            if made != Operand::Local(dest) {
                lowering.emit(InstKind::Copy { dest, value: made }, span);
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// Whether a `where` rule looks at anything along these fields from a name.
    fn rules_along(&self, root: &str, path: &[&str]) -> bool {
        let Some(local) = self.lookup(root) else { return true };
        let mut ty = self.local_type(local);
        for field in path {
            let Type::Named(owner) = &ty else { return true };
            let copy = self.context.type_instance(owner);
            let base = copy.as_ref().map_or(owner.as_str(), |copy| copy.base.as_str());
            if self.context.rules.contains_key(base) {
                return true;
            }
            let Ok(fields) = self.struct_fields(owner) else { return true };
            let Some((_, next)) = fields.iter().find(|(name, _)| name == field) else { return true };
            ty = next.clone();
        }
        false
    }

    /// Runs `body` for each item of what `iter` gives. A `range` going one step at a time is
    /// counted, and anything else but a list is gone through as a list of its items.
    pub(super) fn looping<F>(&mut self, over: Over<'_>, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let Over { names, iter, back } = over;
        if self.is_range(iter)
            && let Some((args, down)) = range_args(iter)
        {
            let (first, limit) = match args.as_slice() {
                [limit] => (Operand::Int(0), self.expr(limit)?),
                [first, limit] => (self.expr(first)?, self.expr(limit)?),
                _ => return Err("it calls `range` with a named argument".to_owned()),
            };
            return self.counting(
                Counting { names, first, limit, down, items: None, back: None },
                span,
                body,
            );
        }
        if let Some(walk) = self.walk_of(iter).filter(|_| back.is_none()) {
            return self.walking(names, walk, span, body);
        }
        if let Some((made, owned)) = self.made_inside(iter)? {
            return self.owning(&owned, Over { names, iter: &made, back }, span, body);
        }
        let ty = self.type_at(iter.span).ok_or("what it loops over has no type")?;
        if let Type::Table(item) = &ty {
            return self.through_slots(names, (iter, (**item).clone()), back.as_ref(), span, body);
        }
        let value = self.expr(iter)?;
        if let Type::Iterator(item, _) = &ty {
            let known = known_generator(iter, &|key| self.context.generators.contains_key(key));
            // `for x in made()? from Wrap(...)`: what the generator fails with is wrapped too.
            let wrap = match &iter.kind {
                ExprKind::Try { context, .. } => context.as_deref(),
                _ => None,
            };
            let item = (**item).clone();
            return self.through_generator(names, (value, item), (known, wrap), (span, iter.span), body);
        }
        if ty == Type::Str && back.is_none() {
            return self.through_text(names, value, span, body);
        }
        let (held, item, conversion) = match &ty {
            // A loop goes through the list as it was, whatever the body does to the name.
            Type::List(item) | Type::Buffer(item) | Type::Array(item, _) => (ty.clone(), *item.clone(), None),
            Type::Str => (Type::List(Box::new(Type::Str)), Type::Str, Some("str.chars")),
            Type::Set(item) | Type::Dict(item, _) => (Type::List(item.clone()), *item.clone(), Some("list")),
            Type::Bytes => (Type::List(Box::new(Type::Int)), Type::Int, Some("list")),
            Type::Named(name) if name == "Args" => (Type::List(Box::new(Type::Str)), Type::Str, Some("list")),
            Type::Named(name) => {
                let found = self.resolve(&format!("{name}.iter"), &[Some(ty.clone())], None);
                let Some((function, found)) = found else {
                    return Err(format!("it has a `for` loop over {name}"));
                };
                if let Type::Iterator(item, _) = &found.returns {
                    let generated = self.temporary(found.returns.clone());
                    let kind = InstKind::Call { dest: Some(generated), function, args: vec![value] };
                    self.emit(kind, span);
                    let source = (Operand::Local(generated), (**item).clone());
                    return self.through_generator(names, source, (None, None), (span, iter.span), body);
                }
                let Type::List(item) = found.returns else {
                    return Err(format!("it has a `for` loop over {name}"));
                };
                let items = self.temporary(Type::List(item.clone()));
                self.emit(InstKind::Call { dest: Some(items), function, args: vec![value] }, span);
                return self.count_through(names, (items, *item), (Operand::Int(0), back), span, body);
            }
            other => return Err(format!("it has a `for` loop over {other}")),
        };
        let items = self.temporary(held);
        self.emit(
            match conversion {
                None => InstKind::Copy { dest: items, value },
                Some(name) => InstKind::Builtin {
                    dest: Some(items),
                    name: name.to_owned(),
                    args: vec![value],
                    changes: false,
                },
            },
            span,
        );
        self.count_through(names, (items, item), (Operand::Int(0), back), span, body)
    }

    /// Goes through a `Table`'s slots in place, with nothing made for it: each slot in use, in
    /// order, read where it is. For `for ref`, the item goes back by its `Id` however the body
    /// leaves, and the `Table` is read from its place each time, so it's never held twice while
    /// it changes. Nothing else changes it in its loop, as the checker sees to, so its number of
    /// slots is read once.
    fn through_slots<F>(
        &mut self,
        names: &[String],
        (iter, item): (&Expr, Type),
        back: Option<&Back>,
        span: Span,
        body: F,
    ) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        // A name or a field of one is read where it is each time, so the loop holds no owner of
        // its own that would outlast it; anything else is worked out once.
        let once = if back.is_some() || names_a_field(iter) { None } else { Some(self.expr(iter)?) };
        let walk = Walk { iter, once, span };
        let stop = self.temporary(Type::Int);
        walk.ask(self, stop, "Table.slots", None)?;
        let count = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: count, value: Operand::Int(0) }, span);
        self.counts.push(crate::Count { count, stop, down: false });
        let (head, then, live, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Jump(head));
        self.current = head;
        let going = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(count), Operand::Local(stop));
        self.emit(InstKind::Binary { dest: going, op: BinOp::Lt, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(going), then, otherwise: exit });
        // A slot whose item has been taken out is stepped over.
        self.current = then;
        let in_use = self.temporary(Type::Bool);
        walk.ask(self, in_use, "Table.slot_live", Some(count))?;
        let skip = self.new_block();
        self.finish(Terminator::Branch { cond: Operand::Local(in_use), then: live, otherwise: skip });
        self.current = skip;
        self.step(count, span);
        self.finish(Terminator::Jump(head));
        self.current = live;
        self.slot_body(names, (&walk, item, count), back, (head, exit), body)?;
        self.current = exit;
        Ok(())
    }

    /// One slot in use: its item under the loop's name, the body, and for `for ref` the item put
    /// back by its `Id`, then on to the next slot.
    fn slot_body<F>(
        &mut self,
        names: &[String],
        (walk, item, count): (&Walk<'_>, Type, LocalId),
        back: Option<&Back>,
        (head, exit): (BlockId, BlockId),
        body: F,
    ) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let span = walk.span;
        self.scopes.push(HashMap::new());
        // For `for ref`, the slot's `Id` is asked first and the item read straight into the
        // loop's name, so nothing reads the `Table` between taking the item and putting it back,
        // and the item can be taken out to change in place, as a list's is.
        let id = match (back, names) {
            (Some(_), [_]) => {
                let id = self.temporary(Type::Id(Box::new(item.clone())));
                walk.ask(self, id, "Table.slot_id", Some(count))?;
                Some(id)
            }
            _ => None,
        };
        let bound = if let [name] = names {
            let dest = self.declare(name, item.clone());
            walk.ask(self, dest, "Table.at_slot", Some(count))
        } else {
            let read = self.temporary(item.clone());
            walk.ask(self, read, "Table.at_slot", Some(count))?;
            self.unpack(names, &Operand::Local(read), span)
        };
        let leaving = match (back, names.first().and_then(|name| self.lookup(name)), id) {
            (Some(back), Some(local), Some(id)) => {
                Some(Leaving::Item { place: back.place.clone(), item: local, index: Operand::Local(id) })
            }
            _ => None,
        };
        let next = self.new_block();
        let broke = if leaving.is_some() { self.new_block() } else { exit };
        self.leaving.extend(leaving.clone());
        self.loops.push(Loop { next, exit: broke, depth: self.leaving.len() });
        let lowered = bound.and_then(|()| body(self));
        self.loops.pop();
        if leaving.is_some() {
            self.leaving.pop();
        }
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        if let Some(leaving) = &leaving {
            self.undo(leaving, span)?;
        }
        self.step(count, span);
        self.finish(Terminator::Jump(head));
        if let Some(leaving) = &leaving {
            self.current = broke;
            self.undo(leaving, span)?;
            self.finish(Terminator::Jump(exit));
        }
        Ok(())
    }

    /// Counts one more.
    fn step(&mut self, count: LocalId, span: Span) {
        let lhs = Operand::Local(count);
        self.emit(InstKind::Binary { dest: count, op: BinOp::Add, lhs, rhs: Operand::Int(1) }, span);
    }

    /// Goes through a list held in a local of its own.
    fn count_through<F>(
        &mut self,
        names: &[String],
        (items, item): (LocalId, Type),
        (first, back): (Operand, Option<Back>),
        span: Span,
        body: F,
    ) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        // An array's length is known here.
        let limit = if let Type::Array(_, size) = self.local_type(items) {
            Operand::Int(i64::try_from(size).map_err(|_| "an array too long to count")?)
        } else {
            let limit = self.temporary(Type::Int);
            let args = vec![Operand::Local(items)];
            let kind =
                InstKind::Builtin { dest: Some(limit), name: "list.len".to_owned(), args, changes: false };
            self.emit(kind, span);
            Operand::Local(limit)
        };
        let counting = Counting { names, first, limit, down: false, items: Some((items, item)), back };
        self.counting(counting, span, body)
    }

    /// Counts in a local of its own, so the loop's name is a fresh value each time around.
    /// The limit is read once. Going one step at a time, the count can't overflow, since
    /// it's short of the limit before each step.
    pub(super) fn counting<F>(&mut self, loop_of: Counting<'_>, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let Counting { names, first, limit, down, items, back } = loop_of;
        let count = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: count, value: first }, span);
        let stop = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: stop, value: limit }, span);
        self.counts.push(crate::Count { count, stop, down });
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
        let by_id = back.as_ref().is_some_and(|back| back.by_id);
        let bound = self.bind_item(names, items, count, by_id, span);
        let item = names.first().and_then(|name| self.lookup(name));
        let leaving = match (back, item, &bound) {
            (Some(back), Some(item), Ok(index)) => {
                Some(Leaving::Item { place: back.place, item, index: index.clone() })
            }
            _ => None,
        };
        let broke = if leaving.is_some() { self.new_block() } else { exit };
        self.leaving.extend(leaving.clone());
        self.loops.push(Loop { next, exit: broke, depth: self.leaving.len() });
        let lowered = bound.and_then(|_| body(self));
        self.loops.pop();
        if leaving.is_some() {
            self.leaving.pop();
        }
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        if let Some(leaving) = &leaving {
            self.undo(leaving, span)?;
        }
        let op = if down { BinOp::Sub } else { BinOp::Add };
        let lhs = Operand::Local(count);
        self.emit(InstKind::Binary { dest: count, op, lhs, rhs: Operand::Int(1) }, span);
        self.finish(Terminator::Jump(head));
        if let Some(leaving) = &leaving {
            self.current = broke;
            self.undo(leaving, span)?;
            self.finish(Terminator::Jump(exit));
        }
        self.current = exit;
        Ok(())
    }

    /// Gives a loop's names their values for this time around. Gives back what its item
    /// is indexed by.
    fn bind_item(
        &mut self,
        names: &[String],
        items: Option<(LocalId, Type)>,
        count: LocalId,
        by_id: bool,
        span: Span,
    ) -> Lowered<Operand> {
        let at = Operand::Local(count);
        let Some((items, ty)) = items else {
            let [name] = names else { return Err("it unpacks a number".to_owned()) };
            let dest = self.declare(name, Type::Int);
            self.emit(InstKind::Copy { dest, value: at.clone() }, span);
            return Ok(at);
        };
        let (object, index) = (Operand::Local(items), at.clone());
        if let ([name], false) = (names, by_id) {
            let dest = self.declare(name, ty);
            self.emit(InstKind::GetIndex { dest, object, index }, span);
            return Ok(at);
        }
        let item = self.temporary(ty.clone());
        self.emit(InstKind::GetIndex { dest: item, object, index }, span);
        let value = Operand::Local(item);
        if let (true, [name], Type::Tuple(parts)) = (by_id, names, &ty) {
            let [id, held] = parts.as_slice() else { return Err("an item has no id".to_owned()) };
            let id_local = self.temporary(id.clone());
            self.emit(InstKind::GetTupleItem { dest: id_local, value: value.clone(), at: 0 }, span);
            let dest = self.declare(name, held.clone());
            self.emit(InstKind::GetTupleItem { dest, value, at: 1 }, span);
            return Ok(Operand::Local(id_local));
        }
        self.unpack(names, &value, span)?;
        Ok(at)
    }

    /// Whether an expression is a call to the built-in `range`.
    pub(super) fn is_range(&self, iter: &Expr) -> bool {
        let ExprKind::Call { callee, .. } = &iter.kind else { return false };
        matches!(&callee.kind, ExprKind::Name(name) if name == "range" && self.lookup(name).is_none())
            && !self.own_function("range")
    }
}

/// The first and limit arguments of a call to `range` with a step of 1 or -1, and
/// whether it counts down.
pub(super) fn range_args(iter: &Expr) -> Option<(Vec<&Expr>, bool)> {
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

/// The function that resumes what a loop goes over, when it can be seen from here.
fn known_generator(iter: &Expr, is_generator: &dyn Fn(&str) -> bool) -> Option<String> {
    let ExprKind::Call { callee, .. } = &iter.kind else { return None };
    let ExprKind::Name(key) = &callee.kind else { return None };
    is_generator(key).then(|| super::generators::resume_name(key))
}

impl Lowering<'_> {
    /// `for c in text`, a character at a time from where the last one ended, so nothing is
    /// made but each character. `str.char_at_byte(text, at)` gives the character starting at
    /// byte `at`, and `str.byte_len(c)` how many bytes it takes.
    fn through_text<F>(&mut self, names: &[String], text: Operand, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let [name] = names else { return Err("it unpacks a string's characters".to_owned()) };
        let builtin = |name: &str, dest, args| InstKind::Builtin {
            dest: Some(dest),
            name: name.to_owned(),
            args,
            changes: false,
        };
        let whole = self.temporary(Type::Str);
        self.emit(InstKind::Copy { dest: whole, value: text }, span);
        let length = self.temporary(Type::Int);
        self.emit(builtin("str.byte_len", length, vec![Operand::Local(whole)]), span);
        // Whether it's all ASCII, worked out once: each character is then its byte, and the loop
        // can be split in two, the ASCII one with nothing to test for each.
        let ascii = self.temporary(Type::Bool);
        self.emit(builtin("str.is_ascii", ascii, vec![Operand::Local(whole)]), span);
        let at = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: at, value: Operand::Int(0) }, span);
        let step = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: step, value: Operand::Int(0) }, span);
        let (head, then, next, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Jump(head));
        self.current = head;
        let going = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(at), Operand::Local(length));
        self.emit(InstKind::Binary { dest: going, op: BinOp::Lt, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(going), then, otherwise: exit });
        self.current = then;
        self.scopes.push(HashMap::new());
        let character = self.declare(name, Type::Str);
        let args = vec![Operand::Local(whole), Operand::Local(at), Operand::Local(ascii)];
        self.emit(builtin("str.char_at_byte", character, args), span);
        self.emit(builtin("str.byte_len", step, vec![Operand::Local(character)]), span);
        self.loops.push(Loop { next, exit, depth: self.leaving.len() });
        let lowered = body(self);
        self.loops.pop();
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        // Where the next character starts: never past the text's end, so it can't overflow.
        let args = vec![Operand::Local(at), Operand::Local(step)];
        self.emit(builtin("str.next_place", at, args), span);
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }
}

/// A loop that goes through what it's over with no list made for it.
enum Unlisted<'e> {
    /// `for x in items[k:]`, a list from a place written as a number, `k`.
    From { items: &'e Expr, item: Type, first: i64 },
    /// `for part in text.split(separator)`, the call written at `call`.
    Split { text: &'e Expr, separator: &'e Expr, call: Span },
}

impl Lowering<'_> {
    /// How a `for` over `iter` can go through it with no list made, if it can.
    fn walk_of<'e>(&self, iter: &'e Expr) -> Option<Unlisted<'e>> {
        match &iter.kind {
            // A list from a place written as a number: its own items from there, with no list
            // copied for the slice. From the end, the slice is made as before.
            ExprKind::Slice { object, start, end: None } => {
                let Some(Type::List(item)) = self.type_at(object.span).and_then(|ty| self.held(&ty)) else {
                    return None;
                };
                Some(Unlisted::From { items: object, item: *item, first: counted_from(start.as_deref())? })
            }
            // A text split by a separator given: a part at a time.
            ExprKind::Call { callee, args } => {
                let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return None };
                let [fennec_syntax::Arg { name: None, is_mut: false, value: separator }] = args.as_slice()
                else {
                    return None;
                };
                let texts = self.type_at(object.span) == Some(Type::Str)
                    && self.type_at(separator.span) == Some(Type::Str);
                (name == "split" && texts).then_some(Unlisted::Split {
                    text: object,
                    separator,
                    call: iter.span,
                })
            }
            _ => None,
        }
    }

    /// A loop through what [`Self::walk_of`] found, with no list made for it.
    fn walking<F>(&mut self, names: &[String], walk: Unlisted<'_>, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        match walk {
            // The loop holds the list as it was, as the slice would have, whatever its body does
            // to the name. Past the end there's nothing, as the slice would have been empty.
            Unlisted::From { items, item, first } => {
                let value = self.expr(items)?;
                let held = self.temporary(Type::List(Box::new(item.clone())));
                self.emit(InstKind::Copy { dest: held, value }, span);
                self.count_through(names, (held, item), (Operand::Int(first), None), span, body)
            }
            Unlisted::Split { text, separator, call } => {
                let text = self.expr(text)?;
                let separator = self.expr(separator)?;
                self.through_split(names, (text, separator), (span, call), body)
            }
        }
    }
}

impl Lowering<'_> {
    /// `for part in text.split(separator)`, a part at a time from where the last separator
    /// ended, so nothing is made but each part, and no list of them all, as `split` would make.
    /// `str.split_at(text, separator, at)` gives the byte where the next separator starts from
    /// byte `at`, or the text's length when there's none left, and `intrinsics.text_part` the
    /// part up to it. The last part runs to the text's end, so there's always one more part than
    /// separators, as `split` gives. An empty separator fails as `split` does, by `split` itself.
    fn through_split<F>(
        &mut self,
        names: &[String],
        (text, separator): (Operand, Operand),
        (span, call): (Span, Span),
        body: F,
    ) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let [name] = names else { return Err("it unpacks a split's parts".to_owned()) };
        let builtin = |name: &str, dest, args| InstKind::Builtin {
            dest: Some(dest),
            name: name.to_owned(),
            args,
            changes: false,
        };
        let whole = self.temporary(Type::Str);
        self.emit(InstKind::Copy { dest: whole, value: text }, span);
        let separator_given = separator.clone();
        let between = self.temporary(Type::Str);
        self.emit(InstKind::Copy { dest: between, value: separator }, span);
        let length = self.temporary(Type::Int);
        self.emit(builtin("intrinsics.text_size", length, vec![Operand::Local(whole)]), span);
        let step = self.temporary(Type::Int);
        self.emit(builtin("intrinsics.text_size", step, vec![Operand::Local(between)]), span);
        let (start, head, then, next, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block(), self.new_block());
        // An empty separator: `split` fails for it, and so does this, saying so where the call is.
        // One written as text that isn't empty can't be, so it's not looked at.
        if matches!(&separator_given, Operand::Str(written) if !written.is_empty()) {
            self.finish(Terminator::Jump(start));
        } else {
            let refused = self.new_block();
            let empty = self.temporary(Type::Bool);
            let (lhs, rhs) = (Operand::Local(step), Operand::Int(0));
            self.emit(InstKind::Binary { dest: empty, op: BinOp::Eq, lhs, rhs }, span);
            self.finish(Terminator::Branch { cond: Operand::Local(empty), then: refused, otherwise: start });
            self.current = refused;
            let parts = self.temporary(Type::List(Box::new(Type::Str)));
            let args = vec![Operand::Local(whole), Operand::Local(between)];
            self.emit(builtin("str.split", parts, args), call);
            self.finish(Terminator::Jump(exit));
        }
        self.current = start;
        let at = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: at, value: Operand::Int(0) }, span);
        // Whether the part just taken ran to the text's end, so it was the last.
        let last = self.temporary(Type::Bool);
        self.emit(InstKind::Copy { dest: last, value: Operand::Bool(false) }, span);
        self.finish(Terminator::Jump(head));
        self.current = head;
        self.finish(Terminator::Branch { cond: Operand::Local(last), then: exit, otherwise: then });
        self.current = then;
        let end = self.temporary(Type::Int);
        let args = vec![Operand::Local(whole), Operand::Local(between), Operand::Local(at)];
        self.emit(builtin("str.split_at", end, args), span);
        let (lhs, rhs) = (Operand::Local(end), Operand::Local(length));
        self.emit(InstKind::Binary { dest: last, op: BinOp::Eq, lhs, rhs }, span);
        self.scopes.push(HashMap::new());
        let part = self.declare(name, Type::Str);
        let args = vec![Operand::Local(whole), Operand::Local(at), Operand::Local(end)];
        self.emit(builtin("intrinsics.text_part", part, args), span);
        self.loops.push(Loop { next, exit, depth: self.leaving.len() });
        let lowered = body(self);
        self.loops.pop();
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        // Past the separator: never past the text's end, since it was found inside it.
        let args = vec![Operand::Local(end), Operand::Local(step)];
        self.emit(builtin("str.next_place", at, args), span);
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }
}

/// The name an expression starts from and the fields it goes through, if that's all it is.
pub(super) fn field_path_of(expr: &Expr) -> Option<(&str, Vec<&str>)> {
    match &expr.kind {
        ExprKind::Name(name) => Some((name, Vec::new())),
        ExprKind::Attr { object, name, optional: false } => {
            let (root, mut path) = field_path_of(object)?;
            path.push(name);
            Some((root, path))
        }
        _ => None,
    }
}

/// Whether what's worked out for an item stays away from the list it's replacing: it doesn't read
/// the name the list is reached from, except for other fields of it, or call a method of it, and
/// it can't leave part of the way along.
fn reads_apart(elem: &Expr, root: &str, first_field: Option<&str>) -> bool {
    let (mut named, mut other_fields, mut apart) = (0_u32, 0_u32, true);
    fennec_syntax::visit::in_expr(&mut elem.clone(), &mut |seen| match &seen.kind {
        ExprKind::Name(name) if name == root => named += 1,
        ExprKind::Attr { object, name, .. }
            if matches!(&object.kind, ExprKind::Name(held) if held == root)
                && Some(name.as_str()) != first_field =>
        {
            other_fields += 1;
        }
        ExprKind::Call { callee, .. }
            if matches!(&callee.kind, ExprKind::Attr { object, .. }
                if matches!(&object.kind, ExprKind::Name(held) if held == root)) =>
        {
            apart = false;
        }
        ExprKind::Try { .. } => apart = false,
        _ => {}
    });
    apart && named == other_fields && (first_field.is_some() || named == 0)
}

/// Whether an expression is a name, or a field of one, which reading again only reads.
fn names_a_field(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(_) => true,
        ExprKind::Attr { object, optional: false, .. } => names_a_field(object),
        _ => false,
    }
}

/// A generator a loop owns besides the one it goes over: one `enumerate` or `zip` reads, made
/// right there. It's let go as the loop is, as `for x in made()?:` lets go of its own.
pub(super) struct Owned {
    generator: Operand,
    known: Option<String>,
    wrap: Option<Expr>,
}

impl Lowering<'_> {
    /// `enumerate(made()?)` or `zip(made()?, ...)`, each generator made right there in it lowered
    /// first and named apart, as what the loop goes over then, with the generators it owns. Each
    /// argument before the last owned one is lowered first too, in order, so they run as written.
    /// Nothing for anything else, a program's own `enumerate` or `zip` among it.
    fn made_inside(&mut self, iter: &Expr) -> Lowered<Option<(Expr, Vec<Owned>)>> {
        let ExprKind::Call { callee, args } = &iter.kind else { return Ok(None) };
        let ExprKind::Name(called) = &callee.kind else { return Ok(None) };
        if !matches!(called.as_str(), "enumerate" | "zip")
            || self.lookup(called).is_some()
            || self.own_function(called)
        {
            return Ok(None);
        }
        let owned_at: Vec<usize> =
            args.iter().enumerate().filter(|(_, arg)| self.made_here(arg)).map(|(at, _)| at).collect();
        let Some(&last) = owned_at.last() else { return Ok(None) };
        let mut made = iter.clone();
        let ExprKind::Call { args, .. } = &mut made.kind else { return Ok(None) };
        let mut owned = Vec::new();
        for (at, arg) in args.iter_mut().enumerate().take(last + 1) {
            let wrap = match &arg.value.kind {
                ExprKind::Try { context, .. } if owned_at.contains(&at) => context.as_deref().cloned(),
                _ => None,
            };
            let known = owned_at.contains(&at).then(|| {
                let call = match &arg.value.kind {
                    ExprKind::Try { value, .. } => value.as_ref(),
                    _ => &arg.value,
                };
                known_generator(call, &|key| self.context.generators.contains_key(key))
            });
            let value = self.expr(&arg.value)?;
            let held = match value {
                Operand::Local(held) => held,
                other => {
                    let ty = self.type_at(arg.value.span).ok_or("an argument has no type")?;
                    let held = self.temporary(ty);
                    self.emit(InstKind::Copy { dest: held, value: other }, arg.value.span);
                    held
                }
            };
            // Named so nothing a program writes names it, and read through that name where it was:
            // an owned generator's name says so, for `enumerate`'s or `zip`'s generator over it.
            let mark = if known.is_some() { OWNED } else { "\0arg" };
            let name = format!("{mark}{}", arg.value.span.start);
            if let Some(scope) = self.scopes.last_mut() {
                scope.insert(name.clone(), held);
            }
            arg.value = Expr { kind: ExprKind::Name(name), span: arg.value.span };
            if let Some(known) = known {
                owned.push(Owned { generator: Operand::Local(held), known, wrap });
            }
        }
        Ok(Some((made, owned)))
    }

    /// Whether an argument to `enumerate` or `zip` is a generator made right there: a call giving
    /// one, or a `?` taken out of one.
    fn made_here(&self, arg: &fennec_syntax::Arg) -> bool {
        let made = match &arg.value.kind {
            ExprKind::Try { value, .. } => value.as_ref(),
            _ => &arg.value,
        };
        arg.name.is_none()
            && matches!(made.kind, ExprKind::Call { .. })
            && matches!(self.type_at(arg.value.span), Some(Type::Iterator(..)))
    }

    /// Runs a loop over what `over` says, owning `owned` too. Each is let go as the loop is left: by
    /// a failure or `return` with what's left then, and otherwise once the loop's done, the last
    /// first, a failure of its own passing up first. Each is left on what's to be left until it's
    /// let go, so a failure letting one go lets go of those before it.
    fn owning<F>(&mut self, owned: &[Owned], over: Over<'_>, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let entered = self.leaving.len();
        for each in owned {
            let wrap = each.wrap.clone().map(Box::new);
            self.leaving.push(Leaving::Generator {
                generator: each.generator.clone(),
                known: each.known.clone(),
                wrap,
            });
        }
        let ran = self.looping(over, span, body);
        if ran.is_err() || !self.is_open() {
            self.leaving.truncate(entered);
            return ran;
        }
        // One that failed partway through, which ends the loop, passes its failure up first, with
        // every one still to be let go, so a failure of another's cleanup is noted on it.
        for each in owned {
            let after = self.new_block();
            self.pass_up_failure(&each.generator, after, span, each.wrap.as_ref())?;
            self.current = after;
        }
        for (at, each) in owned.iter().enumerate().rev() {
            self.leaving.truncate(entered + at);
            self.let_go_now(&each.generator, (each.known.clone(), each.wrap.as_ref()), span)?;
            self.release_generator(&each.generator, span);
        }
        self.leaving.truncate(entered);
        Ok(())
    }
}

/// Whether what `enumerate` or `zip` reads is a generator the loop it's made for owns, which
/// `made_inside` names apart.
pub(super) fn owned_name(read: &Expr) -> bool {
    matches!(&read.kind, ExprKind::Name(name) if name.starts_with(OWNED))
}

/// What starts the name `made_inside` gives a generator the loop owns.
const OWNED: &str = "\0owned";

/// Where a loop over `items[start:]` starts counting: 0 with no start, or the start when it's
/// written as a number that isn't negative. Anything else, like a start counted from the end,
/// is left to the slice.
fn counted_from(start: Option<&Expr>) -> Option<i64> {
    match start.map(|start| &start.kind) {
        None => Some(0),
        Some(ExprKind::Int(first)) if *first >= 0 => Some(*first),
        Some(_) => None,
    }
}
