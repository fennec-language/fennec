//! Lists made natively for a name that holds a native list: `[a, b, c]`, and comprehensions
//! `[item for x in range(...)]`. The runtime makes the list with every item zero, and
//! native code fills it.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, types};
use fennec_syntax::{Arg, BinOp, CompKind, Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind};

use super::Lowerer;
use super::expr::Lowered;
use super::fallback::expr_names;
use super::lists::LIST_ITEMS;
use crate::helpers::Helper;
use crate::types::{ListItem, Ty};

/// A name made up for a value a comprehension builds, which no program can write.
fn hidden(what: &str, span: Span) -> String {
    format!("\0{what}{}", span.start)
}

fn name_expr(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Name(name.to_owned()), span }
}

/// `for names in iter: [if cond:] inner`, the loop a comprehension stands for.
fn comp_loop(names: &[String], iter: &Expr, cond: Option<&Expr>, inner: StmtKind, span: Span) -> Stmt {
    let inner = Stmt { kind: inner, span };
    let body = match cond {
        Some(cond) => {
            let kind = StmtKind::If { branches: vec![(cond.clone(), vec![inner])], otherwise: None };
            vec![Stmt { kind, span }]
        }
        None => vec![inner],
    };
    Stmt { kind: StmtKind::For { names: names.to_vec(), is_mut: false, iter: iter.clone(), body }, span }
}

impl Lowerer<'_, '_> {
    /// A list or dict comprehension made by an ordinary native loop that fills a hidden name,
    /// when native code can hold what it makes.
    pub(super) fn comprehension(&mut self, expr: &Expr) -> Result<Option<Lowered>, Diagnostic> {
        match &expr.kind {
            ExprKind::List(_) => return self.list_literal(expr),
            ExprKind::Dict(_) => return self.dict_literal(expr),
            _ => {}
        }
        let ExprKind::Comprehension { kind, elem, value, names, iter, cond } = &expr.kind else {
            return Ok(None);
        };
        let span = expr.span;
        let cond = cond.as_deref();
        let name = hidden("made", span);
        let target = name_expr(&name, span);
        let (start, ty, inner) = match (kind, value.as_deref(), self.checked_expr(expr)) {
            (CompKind::List, None, Some(ty @ Ty::List(item))) => {
                let element = self.element(item.ty())?;
                let none = self.int(0);
                let append = Expr {
                    kind: ExprKind::Attr {
                        object: Box::new(target),
                        name: "append".to_owned(),
                        optional: false,
                    },
                    span,
                };
                let arg = Arg { name: None, is_mut: false, value: (**elem).clone() };
                let call = Expr { kind: ExprKind::Call { callee: Box::new(append), args: vec![arg] }, span };
                (self.help(Helper::ListNew, &[element, none])?, ty, StmtKind::Expr(call))
            }
            (CompKind::Dict, Some(item), Some(ty @ Ty::Dict(key, word))) => {
                let shape = fennec_interp::embed::dicts::shape(self.shared.tables.str_shape, key, word)
                    .ok_or_else(|| super::internal("a dict's shape", span))?;
                let shape = self.handed_number("shape", shape.cast_signed());
                let room = self.int(0);
                let slot =
                    Expr { kind: ExprKind::Index { object: Box::new(target), index: elem.clone() }, span };
                let assign = StmtKind::Assign {
                    target: slot,
                    op: None,
                    value: item.clone(),
                    declare_mut: false,
                    ty: None,
                };
                (self.help(Helper::DictNew, &[shape, room])?, ty, assign)
            }
            _ => return Ok(None),
        };
        self.open_scope();
        self.bind_native(&name, ty, Some(start), true);
        self.stmt_inside_expr(&comp_loop(names, iter, cond, inner, span))?;
        let made = self.local(&name).and_then(|local| local.var).map(|var| self.b.use_var(var));
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.remove(&name);
        }
        self.close_scope()?;
        Ok(Some((made, ty)))
    }

    /// `sum(items)` with a start or without, made by an ordinary native loop adding to a
    /// hidden name, when it's a sum of ints or floats.
    pub(super) fn native_sum(&mut self, call: &Expr, args: &[Arg]) -> Result<Option<Lowered>, Diagnostic> {
        let (Some(first), true) = (args.first(), args.len() <= 2) else { return Ok(None) };
        if args.iter().any(|arg| arg.name.is_some() || arg.is_mut) {
            return Ok(None);
        }
        let Some(ty @ (Ty::Int | Ty::Float)) = self.checked_expr(call) else { return Ok(None) };
        let span = call.span;
        let start = match args.get(1) {
            Some(start) => self.expr(&start.value)?,
            None => self.expr(&Expr { kind: ExprKind::Int(0), span })?,
        };
        let start = self.convert(start.0, start.1, ty, span)?;
        let name = hidden("sum", span);
        let target = name_expr(&name, span);
        let add = |value: &Expr| StmtKind::Assign {
            target: target.clone(),
            op: Some(BinOp::Add),
            value: value.clone(),
            declare_mut: false,
            ty: None,
        };
        let each = match &first.value.kind {
            ExprKind::Comprehension { kind: CompKind::Generator, elem, value: None, names, iter, cond } => {
                comp_loop(names, iter, cond.as_deref(), add(elem), span)
            }
            ExprKind::Comprehension { .. } => return Ok(None),
            _ => {
                let item = hidden("item", span);
                comp_loop(std::slice::from_ref(&item), &first.value, None, add(&name_expr(&item, span)), span)
            }
        };
        self.open_scope();
        self.bind_native(&name, ty, Some(start), true);
        self.stmt_inside_expr(&each)?;
        let total = self.local(&name).and_then(|local| local.var).map(|var| self.b.use_var(var));
        self.close_scope()?;
        Ok(Some((total, ty)))
    }

    /// `sum(...)`, when the call is to the built-in one.
    pub(super) fn sum_call(
        &mut self,
        call: &Expr,
        callee: &Expr,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let builtin = matches!(&callee.kind, ExprKind::Name(name) if name == "sum" && self.local(name).is_none())
            && !self.shared.compiled.contains_key("sum");
        if builtin { self.native_sum(call, args) } else { Ok(None) }
    }

    /// `sorted(items)` when the list holds numbers, or `sorted(items, key=(item) => ...)` when
    /// the keys are numbers, for any native list. The runtime does the sorting.
    pub(super) fn sorted_call(
        &mut self,
        call: &Expr,
        callee: &Expr,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let builtin = matches!(&callee.kind, ExprKind::Name(name) if name == "sorted" && self.local(name).is_none())
            && !self.shared.compiled.contains_key("sorted");
        let (true, Some(ty @ Ty::List(item)), Some(items)) = (builtin, self.checked_expr(call), args.first())
        else {
            return Ok(None);
        };
        if items.name.is_some() || items.is_mut || self.checked_expr(&items.value) != Some(ty) {
            return Ok(None);
        }
        match args {
            [_] if matches!(item, ListItem::Int | ListItem::Float | ListItem::Bool) => {
                let (value, from) = self.expr(&items.value)?;
                let list = self.convert(value, from, ty, call.span)?;
                let float = self.int(i64::from(item == ListItem::Float));
                let made = self.help(Helper::ListSorted, &[list, float])?;
                self.consume(Some(list), ty)?;
                Ok(Some((Some(made), ty)))
            }
            [
                _,
                Arg {
                    name: Some(key),
                    is_mut: false,
                    value: Expr { kind: ExprKind::Lambda { params, body }, .. },
                },
            ] if key == "key" => {
                let [param] = params.as_slice() else { return Ok(None) };
                self.sorted_by(&items.value, param, body, ty, call.span)
            }
            _ => Ok(None),
        }
    }

    /// A list sorted by keys that an ordinary native loop works out first, in order, as the
    /// interpreter does.
    fn sorted_by(
        &mut self,
        items: &Expr,
        param: &str,
        body: &Expr,
        ty: Ty,
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let Some(key @ (Ty::Int | Ty::Float | Ty::Bool)) = self.checked_expr(body) else {
            return Ok(None);
        };
        let Some(key_item) = ListItem::of(key) else { return Ok(None) };
        let (value, from) = self.expr(items)?;
        let list = self.convert(value, from, ty, span)?;
        let list = self.own(list, ty);
        let (items_name, keys_name) = (hidden("items", span), hidden("keys", span));
        let element = self.element(key)?;
        let none = self.int(0);
        let keys = self.help(Helper::ListNew, &[element, none])?;
        self.open_scope();
        self.bind_native(&items_name, ty, Some(list), false);
        self.bind_native(&keys_name, Ty::List(key_item), Some(keys), true);
        let append = Expr {
            kind: ExprKind::Attr {
                object: Box::new(name_expr(&keys_name, span)),
                name: "append".to_owned(),
                optional: false,
            },
            span,
        };
        let arg = Arg { name: None, is_mut: false, value: body.clone() };
        let call = Expr { kind: ExprKind::Call { callee: Box::new(append), args: vec![arg] }, span };
        let each =
            comp_loop(&[param.to_owned()], &name_expr(&items_name, span), None, StmtKind::Expr(call), span);
        self.stmt_inside_expr(&each)?;
        let keys = self.local(&keys_name).and_then(|local| local.var).map(|var| self.b.use_var(var));
        let keys = keys.ok_or_else(|| super::internal("a lost list", span))?;
        let float = self.int(i64::from(key == Ty::Float));
        let made = self.help(Helper::ListSortedBy, &[list, keys, float])?;
        self.close_scope()?;
        Ok(Some((Some(made), ty)))
    }

    /// Lowers a statement in the middle of an expression. Statements drop temporaries, so
    /// this drops only back to here, keeping the ones made earlier in the expression.
    fn stmt_inside_expr(&mut self, stmt: &Stmt) -> Result<(), Diagnostic> {
        self.keeping_temps(|lowerer| lowerer.stmt(stmt))
    }

    /// Lowers statements in the middle of an expression, like a body inlined there.
    /// Statements drop temporaries, so these drop only back to here, keeping the ones made
    /// earlier in the expression.
    pub(super) fn keeping_temps<T>(
        &mut self,
        lower: impl FnOnce(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        let held = self.frame && self.dirty;
        let mark = if held { Some(self.help(Helper::Mark, &[])?) } else { None };
        self.bases.extend(mark);
        let lowered = lower(self);
        if held {
            self.bases.pop();
            self.dirty = true;
        }
        lowered
    }

    /// `[a, b, c]` made natively, when the name it's for holds a native list, or `None`.
    /// Items are worked out in order, as the interpreter does.
    pub(super) fn list_literal_for(
        &mut self,
        name: &str,
        value: &Expr,
    ) -> Result<Option<Lowered>, Diagnostic> {
        match self.checked_ty(name) {
            Some(ty) => self.list_literal_as(value, ty),
            None => Ok(None),
        }
    }

    /// `[a, b, c]` made natively, when the checker knows it can be.
    fn list_literal(&mut self, value: &Expr) -> Result<Option<Lowered>, Diagnostic> {
        match self.checked_expr(value) {
            Some(ty) => self.list_literal_as(value, ty),
            None => Ok(None),
        }
    }

    fn list_literal_as(&mut self, value: &Expr, ty: Ty) -> Result<Option<Lowered>, Diagnostic> {
        let (ExprKind::List(items), Ty::List(item)) = (&value.kind, ty) else {
            return Ok(None);
        };
        let mut lowered = Vec::with_capacity(items.len());
        for each in items {
            let (value, from) = self.expr(each)?;
            lowered.push(self.convert(value, from, item.ty(), each.span)?);
        }
        let element = self.element(item.ty())?;
        let count = self.int(i64::try_from(items.len()).unwrap_or(0));
        let list = self.help(Helper::ListNew, &[element, count])?;
        let stride = self.stride(item.ty())?;
        for (at, value) in lowered.into_iter().enumerate() {
            let offset = i64::try_from(at).unwrap_or(0) * stride + LIST_ITEMS;
            let address = self.b.ins().iadd_imm_s(list, offset);
            self.store_item(address, item.ty(), (Some(value), item.ty()), value_span(items, at))?;
        }
        Ok(Some((Some(list), ty)))
    }

    /// Declares `name` as `[item for x in range(...)]` natively, when it holds a packed list
    /// of ints, floats or bools. Gives back `false`, having done nothing, when it can't.
    pub(super) fn packed_comprehension(
        &mut self,
        name: &str,
        value: &Expr,
        is_mut: bool,
    ) -> Result<bool, Diagnostic> {
        let ExprKind::Comprehension { kind: CompKind::List, elem, value: None, names, iter, cond: None } =
            &value.kind
        else {
            return Ok(false);
        };
        let ([var], ExprKind::Call { callee, args }) = (names.as_slice(), &iter.kind) else {
            return Ok(false);
        };
        let is_range = matches!(&callee.kind, ExprKind::Name(callee) if callee == "range" && self.local(callee).is_none());
        if !is_range
            || !(1..=2).contains(&args.len())
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
        {
            return Ok(false);
        }
        let Some(Ty::List(list_item @ (ListItem::Int | ListItem::Float | ListItem::Bool))) =
            self.checked_ty(name)
        else {
            return Ok(false);
        };
        let item = list_item.ty();
        // An item that refers to the name sees what it held before, so leave that to the
        // interpreter.
        let mut mentioned = Vec::new();
        expr_names(elem, &mut mentioned);
        if mentioned.iter().any(|mentioned| mentioned == name) {
            return Ok(false);
        }
        let mut bounds = Vec::with_capacity(args.len());
        for arg in args {
            let (bound, ty) = self.expr(&arg.value)?;
            bounds.push(self.convert(bound, ty, Ty::Int, arg.value.span)?);
        }
        let zero = self.int(0);
        let (start, end) = match bounds.as_slice() {
            [end] => (zero, *end),
            [start, end] => (*start, *end),
            _ => return Ok(false),
        };
        let difference = self.b.ins().isub(end, start);
        let count = self.b.ins().smax(difference, zero);
        let element = self.element(item)?;
        let made = self.help(Helper::ListNew, &[element, count])?;
        self.declare(name, Some(made), Ty::List(list_item), is_mut, value)?;
        if !is_zero(elem) {
            self.fill(name, item, (var, start, count), elem)?;
        }
        Ok(true)
    }

    /// Works `elem` out for each `var` from `start` on, storing it into each of the `count`
    /// items of a packed list nothing else refers to yet.
    fn fill(
        &mut self,
        name: &str,
        item: Ty,
        (var, start, count): (&str, cranelift_codegen::ir::Value, cranelift_codegen::ir::Value),
        elem: &Expr,
    ) -> Result<(), Diagnostic> {
        let items = self.items_at(name, item, true)?;
        self.drop_temps()?;
        let counter = self.b.declare_var(types::I64);
        let zero = self.int(0);
        self.b.def_var(counter, zero);
        let [header, inside, exit] = [(); 3].map(|()| self.b.create_block());
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let at = self.b.use_var(counter);
        let more = self.b.ins().icmp(IntCC::SignedLessThan, at, count);
        self.b.ins().brif(more, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        self.open_scope();
        let x = self.b.ins().iadd(start, at);
        self.bind_native(var, Ty::Int, Some(x), false);
        let lowered = self.expr(elem)?;
        let offset = self.b.ins().ishl_imm_s(at, 3);
        let address = self.b.ins().iadd(items, offset);
        self.store_item(address, item, lowered, elem.span)?;
        self.drop_temps()?;
        self.close_scope()?;
        let next = self.b.ins().iadd_imm_s(at, 1);
        self.b.def_var(counter, next);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(exit);
        Ok(())
    }
}

fn value_span(items: &[Expr], at: usize) -> Span {
    items.get(at).map(|item| item.span).unwrap_or_default()
}

/// Whether an item is a zero the runtime's new list already holds.
fn is_zero(elem: &Expr) -> bool {
    match &elem.kind {
        ExprKind::Int(n) => *n == 0,
        ExprKind::Float(x) => x.to_bits() == 0,
        ExprKind::Bool(b) => !b,
        _ => false,
    }
}
