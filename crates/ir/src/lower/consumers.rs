//! What uses a generator written right inside it, as a loop with nothing collected: `sum`,
//! `any` and `all`. Over a stage, the loop fuses like any other.

use fennec_check::Type;
use fennec_syntax::{Arg, BinOp, CompKind, Expr, ExprKind, Span, Stmt, StmtKind};

use super::{Lowered, Lowering, Tail};
use crate::{InstKind, Operand};

/// A generator written in place, in its parts.
struct Written<'e> {
    elem: &'e Expr,
    names: &'e [String],
    iter: &'e Expr,
    cond: Option<&'e Expr>,
}

fn written(value: &Expr) -> Option<Written<'_>> {
    let ExprKind::Comprehension { kind: CompKind::Generator, elem, names, iter, cond, .. } = &value.kind
    else {
        return None;
    };
    Some(Written { elem, names, iter, cond: cond.as_deref() })
}

fn stmt(kind: StmtKind, span: Span) -> Stmt {
    Stmt { kind, span }
}

fn name_at(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Name(name.to_owned()), span }
}

/// `name = value`, for a name made here, whose span says so by matching its value's.
fn binding(name: &str, value: Expr, span: Span) -> Stmt {
    let target = name_at(name, value.span);
    stmt(StmtKind::Assign { target, op: None, value, declare_mut: true, ty: None }, span)
}

fn setting(name: &str, to: bool, span: Span) -> Stmt {
    let value = Expr { kind: ExprKind::Bool(to), span };
    let target = name_at(name, span);
    let kind = StmtKind::Assign { target, op: None, value, declare_mut: false, ty: None };
    stmt(kind, span)
}

impl Lowering<'_> {
    /// `sum`, `any` or `all` of a generator written in place, if this is one.
    pub(super) fn consumer(&mut self, expr: &Expr, callee: &Expr, args: &[Arg]) -> Lowered<Option<Operand>> {
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        if self.lookup(name).is_some() || self.context.functions.contains(name.as_str()) {
            return Ok(None);
        }
        let Some(Arg { name: None, is_mut: false, value }) = args.first() else { return Ok(None) };
        match (name.as_str(), args) {
            ("enumerate", [_]) if self.gives_items(value) => {
                return self.enumerated_generator(expr, value).map(Some);
            }
            ("zip", [_, second]) if self.gives_items(value) || self.gives_items(&second.value) => {
                return self.zipped_generator(expr, value, &second.value).map(Some);
            }
            _ => {}
        }
        // A generator kept somewhere is gone through like one written here.
        let item = format!("\0item{}", value.span.start);
        let names = [item.clone()];
        let read = name_at(&item, Span { start: value.span.start, end: value.span.start });
        let parts = match written(value) {
            Some(parts) => parts,
            None if self.gives_items(value)
                || (name == "sum" && (self.adds_its_own_way(value) || self.is_a_view(value))) =>
            {
                Written { elem: &read, names: &names, iter: value, cond: None }
            }
            None => return Ok(None),
        };
        // A sum, its start written or the `0.0` the checker writes in for floats.
        let summed = name == "sum"
            && matches!(args, [_] | [_, Arg { is_mut: false, .. }])
            && args.get(1).is_none_or(|start| start.name.as_deref().is_none_or(|named| named == "start"));
        // Over `parallel(xs)`, when the backend runs it on threads.
        if summed {
            let start = super::parallel::Gives::Sum(args.get(1).map(|start| &start.value));
            if let Some(outlined) =
                self.parallel_value(expr, (parts.elem, parts.names, parts.iter, parts.cond), start)
            {
                return outlined.map(Some);
            }
        }
        if summed {
            return self.summing(expr, &parts, args.get(1));
        }
        match (name.as_str(), args) {
            ("any" | "all", [_]) => self.deciding(name == "any", &parts, expr.span).map(Some),
            _ => Ok(None),
        }
    }

    /// Whether this is a dict's or a `Table`'s `values()` or `keys()`, which a loop goes through
    /// where they are, so a `sum` of one is that loop rather than a list made to add up.
    fn is_a_view(&self, value: &Expr) -> bool {
        let ExprKind::Call { callee, args } = &value.kind else { return false };
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return false };
        args.is_empty()
            && matches!(name.as_str(), "values" | "keys")
            && matches!(self.type_at(object.span), Some(Type::Dict(..) | Type::Table(_)))
    }

    /// Whether this is a list of what `+` on its items isn't an int's or a float's.
    fn adds_its_own_way(&self, value: &Expr) -> bool {
        matches!(self.type_at(value.span), Some(Type::List(item)) if !matches!(*item, Type::Int | Type::Float))
    }

    /// Whether what's written gives a generator, or is a type with an `iter` of its own, which a
    /// `for` loop goes through the same way: `list(queue)` is the list of what `queue.iter()` gives.
    fn gives_items(&self, value: &Expr) -> bool {
        match self.type_at(value.span) {
            Some(Type::Iterator(_)) => true,
            Some(ty) => self.own_iter_items(&ty).is_some(),
            None => false,
        }
    }

    /// What a type's own `iter` gives, when it has one that's a generator.
    pub(super) fn own_iter_items(&self, ty: &Type) -> Option<Type> {
        let Type::Named(name) = ty else { return None };
        let (_, found) = self.resolve(&format!("{name}.iter"), &[Some(ty.clone())], None)?;
        let Type::Iterator(item) = found.returns else { return None };
        Some(*item)
    }

    /// The arguments for what uses every item of a generator and works on a list: the
    /// generator's items, collected right here.
    pub(super) fn collected_args(&self, callee: &Expr, args: &[Arg]) -> Option<Vec<Arg>> {
        let takes_all = match &callee.kind {
            ExprKind::Name(name) => {
                self.lookup(name).is_none()
                    && !self.context.functions.contains(name.as_str())
                    && matches!(
                        name.as_str(),
                        "min" | "max" | "sorted" | "list" | "set" | "dict" | "reversed"
                    )
            }
            ExprKind::Attr { name, .. } => name == "join",
            _ => false,
        };
        let first =
            args.first().filter(|first| takes_all && !first.is_mut && self.gives_items(&first.value))?;
        let span = first.value.span;
        let collected =
            if let ExprKind::Comprehension { kind: CompKind::Generator, elem, names, iter, cond, .. } =
                &first.value.kind
            {
                ExprKind::Comprehension {
                    kind: CompKind::List,
                    elem: elem.clone(),
                    value: None,
                    names: names.clone(),
                    iter: iter.clone(),
                    cond: cond.clone(),
                }
            } else {
                let item = format!("\0item{}", span.start);
                let read = name_at(&item, Span { start: span.start, end: span.start });
                ExprKind::Comprehension {
                    kind: CompKind::List,
                    elem: Box::new(read),
                    value: None,
                    names: vec![item],
                    iter: Box::new(first.value.clone()),
                    cond: None,
                }
            };
        let mut rewritten = args.to_vec();
        if let Some(slot) = rewritten.first_mut() {
            slot.value = Expr { kind: collected, span };
        }
        Some(rewritten)
    }

    fn summing(&mut self, expr: &Expr, parts: &Written<'_>, start: Option<&Arg>) -> Lowered<Option<Operand>> {
        let ty = self.ty(expr)?;
        // Anything but a number has its start written.
        if !matches!(ty, Type::Int | Type::Float) && start.is_none() {
            return Ok(None);
        }
        let span = Span { start: expr.span.start, end: expr.span.start };
        let total = format!("\0sum{}", expr.span.start);
        let first = start.map_or_else(
            || Expr { kind: if ty == Type::Float { ExprKind::Float(0.0) } else { ExprKind::Int(0) }, span },
            |start| start.value.clone(),
        );
        if ty == Type::Float {
            return self.summing_floats(&total, first, parts, expr.span).map(Some);
        }
        let target = name_at(&total, expr.span);
        let add = StmtKind::Assign {
            target,
            op: Some(BinOp::Add),
            value: parts.elem.clone(),
            declare_mut: false,
            ty: None,
        };
        let body = with_condition(parts.cond, vec![stmt(add, expr.span)], expr.span);
        // A sum of sized numbers is held at their size from its start, a number written out.
        if matches!(ty, Type::Sized(_)) {
            let value = self.expr(&first)?;
            let value = self.coerce(value, &ty, span)?;
            let held = self.declare(&total, ty);
            self.emit(InstKind::Copy { dest: held, value }, span);
            let looped =
                StmtKind::For { names: parts.names.to_vec(), is_mut: false, iter: parts.iter.clone(), body };
            self.stmts(&[stmt(looped, expr.span)], Tail::Drop)?;
            return Ok(Some(Operand::Local(held)));
        }
        self.looping_for_value(&total, first, parts, body, expr.span).map(Some)
    }

    /// A sum of floats, in the fixed order `fennec_syntax::fixed_sum` writes, so it's the same
    /// whatever the items are and wherever it runs.
    fn summing_floats(
        &mut self,
        total: &str,
        start: Expr,
        parts: &Written<'_>,
        at: Span,
    ) -> Lowered<Operand> {
        // What's written compares only ints, at a place of its own: the end of the call.
        let compared = Span { start: at.end, end: at.end };
        if let Some(unrolled) = self.summing_unrolled(total, &start, parts, at) {
            return unrolled;
        }
        let point = Span { start: at.start, end: at.start };
        let places = fennec_syntax::fixed_sum::Places { at: point, compared, counted: point, item: point };
        let written = fennec_syntax::fixed_sum::fixed_sum(total, start, parts.elem, places);
        let body = with_condition(parts.cond, written.each, at);
        let looped =
            StmtKind::For { names: parts.names.to_vec(), is_mut: false, iter: parts.iter.clone(), body };
        let mut all = written.setup;
        all.push(stmt(looped, at));
        all.extend(written.finish);
        self.retyped.push((compared, Type::Bool));
        let done = self.stmts(&all, Tail::Drop);
        self.retyped.pop();
        done?;
        let local = self.lookup(total).ok_or("a sum lost its value")?;
        Ok(Operand::Local(local))
    }

    /// A float sum with no `if`, over a `range` counting up by one or over a list, written out
    /// eight items a round, so each of the eight totals adds without waiting on the others.
    fn summing_unrolled(
        &mut self,
        total: &str,
        start: &Expr,
        parts: &Written<'_>,
        at: Span,
    ) -> Option<Lowered<Operand>> {
        use fennec_syntax::fixed_sum::{Over, Places, unrolled};
        let ([name], None) = (parts.names, parts.cond) else { return None };
        let list_item = match self.type_at(parts.iter.span) {
            Some(Type::List(item)) => Some(*item),
            _ => None,
        };
        let over = if self.is_range(parts.iter) {
            match super::lists::range_args(parts.iter)? {
                (bounds, false) => match bounds.as_slice() {
                    [limit] => Over::Range { first: None, limit: (*limit).clone() },
                    [first, limit] => Over::Range { first: Some((*first).clone()), limit: (*limit).clone() },
                    _ => return None,
                },
                (_, true) => return None,
            }
        } else if list_item.is_some() {
            Over::List(parts.iter.clone())
        } else {
            return None;
        };
        // Each place what's written is typed from has a span of its own, inside `sum` itself.
        let place = |by: u32| Span { start: at.start + by, end: at.start + by };
        let places = Places {
            at: place(0),
            compared: Span { start: at.end, end: at.end },
            counted: place(1),
            item: place(2),
        };
        let written = unrolled(total, start.clone(), (name, parts.elem), &over, places);
        let before = self.retyped.len();
        self.retyped.push((places.compared, Type::Bool));
        self.retyped.push((places.counted, Type::Int));
        if let Some(item) = list_item {
            self.retyped.push((places.item, item));
        }
        let done = self.stmts(&written, Tail::Drop);
        self.retyped.truncate(before);
        Some(done.and_then(|()| {
            self.lookup(total).map(Operand::Local).ok_or_else(|| "a sum lost its value".to_owned())
        }))
    }

    fn deciding(&mut self, any: bool, parts: &Written<'_>, at: Span) -> Lowered<Operand> {
        let span = Span { start: at.start, end: at.start };
        let found = format!("\0decided{}", at.start);
        let first = Expr { kind: ExprKind::Bool(!any), span };
        let leave = vec![setting(&found, any, span), stmt(StmtKind::Break, span)];
        let decide = if any {
            StmtKind::If { branches: vec![(parts.elem.clone(), leave)], otherwise: None }
        } else {
            let pass = vec![stmt(StmtKind::Pass, span)];
            StmtKind::If { branches: vec![(parts.elem.clone(), pass)], otherwise: Some(leave) }
        };
        let body = with_condition(parts.cond, vec![stmt(decide, span)], span);
        self.looping_for_value(&found, first, parts, body, span)
    }

    /// Gives a hidden name its first value, runs the loop, and gives back what it ends up holding.
    fn looping_for_value(
        &mut self,
        name: &str,
        first: Expr,
        parts: &Written<'_>,
        body: Vec<Stmt>,
        span: Span,
    ) -> Lowered<Operand> {
        let start = binding(name, first, span);
        let looped =
            StmtKind::For { names: parts.names.to_vec(), is_mut: false, iter: parts.iter.clone(), body };
        self.stmts(&[start, stmt(looped, span)], Tail::Drop)?;
        let local = self.lookup(name).ok_or("a consumer lost its value")?;
        Ok(Operand::Local(local))
    }
}

fn with_condition(cond: Option<&Expr>, body: Vec<Stmt>, span: Span) -> Vec<Stmt> {
    match cond {
        Some(cond) => {
            let kind = StmtKind::If { branches: vec![(cond.clone(), body)], otherwise: None };
            vec![stmt(kind, span)]
        }
        None => body,
    }
}
