//! Lists made natively for a name that holds a native list: `[a, b, c]`, and comprehensions
//! `[item for x in range(...)]`. The runtime makes the list with every item zero, and
//! native code fills it.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Value, types};
use fennec_syntax::{Arg, BinOp, CompKind, Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind};

use fennec_interp::embed::lists::{SORT_FLOATS, SORT_INTS, SORT_STRINGS};

use super::Lowerer;
use super::expr::Lowered;
use super::fallback::expr_names;
use super::lists::LIST_ITEMS;
use crate::helpers::Helper;
use crate::types::{ListItem, Ty, Word};

/// How the runtime orders words of a kind: as ints, floats or strings.
fn sort_kind(ty: Ty) -> i64 {
    match ty {
        Ty::Float => SORT_FLOATS,
        Ty::Str => SORT_STRINGS,
        _ => SORT_INTS,
    }
}

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
            ExprKind::Set(_) => return self.set_literal(expr),
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
            (CompKind::Set, None, Some(ty @ Ty::Dict(key, Word::Nothing))) => {
                let shape =
                    fennec_interp::embed::dicts::shape(self.shared.tables.str_shape, key, Word::Nothing)
                        .ok_or_else(|| super::internal("a set's shape", span))?;
                let shape = self.handed_number("shape", shape.cast_signed());
                let room = self.int(0);
                let add = Expr {
                    kind: ExprKind::Attr {
                        object: Box::new(target),
                        name: "add".to_owned(),
                        optional: false,
                    },
                    span,
                };
                let arg = Arg { name: None, is_mut: false, value: (**elem).clone() };
                let call = Expr { kind: ExprKind::Call { callee: Box::new(add), args: vec![arg] }, span };
                (self.help(Helper::DictNew, &[shape, room])?, ty, StmtKind::Expr(call))
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

    /// `any(...)` or `all(...)` of a generator, made by an ordinary native loop that stops at
    /// the first item that settles it, when the call is to the built-in one.
    pub(super) fn any_or_all_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        let any = name == "any";
        if (!any && name != "all") || self.local(name).is_some() || self.shared.compiled.contains_key(name) {
            return Ok(None);
        }
        let [
            Arg {
                name: None,
                is_mut: false,
                value:
                    Expr {
                        kind:
                            ExprKind::Comprehension {
                                kind: CompKind::Generator,
                                elem,
                                value: None,
                                names,
                                iter,
                                cond,
                            },
                        span,
                    },
            },
        ] = args
        else {
            return Ok(None);
        };
        if self.checked_expr(elem) != Some(Ty::Bool) {
            return Ok(None);
        }
        let span = *span;
        let answer = hidden("answer", span);
        let start = self.b.ins().iconst(types::I8, i64::from(!any));
        let settled = Stmt {
            kind: StmtKind::Assign {
                target: name_expr(&answer, span),
                op: None,
                value: Expr { kind: ExprKind::Bool(any), span },
                declare_mut: false,
                ty: None,
            },
            span,
        };
        let test = if any {
            (**elem).clone()
        } else {
            Expr {
                kind: ExprKind::Unary { op: fennec_syntax::UnaryOp::Not, expr: elem.clone() },
                span: elem.span,
            }
        };
        let stop = StmtKind::If {
            branches: vec![(test, vec![settled, Stmt { kind: StmtKind::Break, span }])],
            otherwise: None,
        };
        self.open_scope();
        self.bind_native(&answer, Ty::Bool, Some(start), true);
        self.stmt_inside_expr(&comp_loop(names, iter, cond.as_deref(), stop, span))?;
        let made = self.local(&answer).and_then(|local| local.var).map(|var| self.b.use_var(var));
        self.close_scope()?;
        Ok(Some((made, Ty::Bool)))
    }

    /// `sorted(items)` when the list holds numbers or strings, or `sorted(items, key=(item) =>
    /// ...)` when the keys are, for any native list. The runtime does the sorting.
    pub(super) fn sorted_call(
        &mut self,
        call: &Expr,
        callee: &Expr,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let builtin = matches!(&callee.kind, ExprKind::Name(name) if name == "sorted" && self.local(name).is_none())
            && !self.shared.compiled.contains_key("sorted");
        let (true, Some(ty @ Ty::List(_)), Some(items)) = (builtin, self.checked_expr(call), args.first())
        else {
            return Ok(None);
        };
        if items.name.is_some() || items.is_mut || self.checked_expr(&items.value) != Some(ty) {
            return Ok(None);
        }
        self.sorted_as(&items.value, args.get(1..).unwrap_or_default(), ty, call.span)
    }

    /// A list sorted, given the arguments after the list, or `None` when native code can't.
    pub(super) fn sorted_as(
        &mut self,
        items: &Expr,
        args: &[Arg],
        ty: Ty,
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let Ty::List(item) = ty else { return Ok(None) };
        match args {
            [] if !matches!(item, ListItem::Struct(_) | ListItem::Boxed(_)) => {
                let (value, from) = self.expr(items)?;
                let list = self.convert(value, from, ty, span)?;
                let kind = self.int(sort_kind(item.ty()));
                let made = self.help(Helper::ListSorted, &[list, kind])?;
                self.consume(Some(list), ty)?;
                Ok(Some((Some(made), ty)))
            }
            [
                Arg {
                    name: Some(key),
                    is_mut: false,
                    value: Expr { kind: ExprKind::Lambda { params, body }, .. },
                },
            ] if key == "key" => {
                let [param] = params.as_slice() else { return Ok(None) };
                self.sorted_by(items, param, body, ty, span)
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
        let Some(key @ (Ty::Int | Ty::Float | Ty::Bool | Ty::Str)) = self.checked_expr(body) else {
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
        let kind = self.int(sort_kind(key));
        let made = self.help(Helper::ListSortedBy, &[list, keys, kind])?;
        self.close_scope()?;
        Ok(Some((Some(made), ty)))
    }

    /// Lowers something that needs a list by name, with a native list a call or the like
    /// gave held under a hidden one until it's done. A list that's lent keeps its owners.
    pub(super) fn holding_list<T>(
        &mut self,
        (list, ty): (Value, Ty),
        lent: bool,
        span: Span,
        lower: impl FnOnce(&mut Self, &Expr) -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        let name = hidden("held", span);
        self.open_scope();
        if lent && self.borrowed.contains(&list) {
            self.bind_lent(&name, (Some(list), ty))?;
        } else {
            let list = self.own(list, ty);
            self.bind_native(&name, ty, Some(list), false);
        }
        let lowered = lower(self, &name_expr(&name, span));
        self.close_scope()?;
        lowered
    }

    /// A new list of the items a list a name holds gives, for which `test` is true.
    pub(super) fn filtered(
        &mut self,
        name: &str,
        param: &str,
        test: &Expr,
        item: ListItem,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let kept = hidden("kept", span);
        let element = self.element(item.ty())?;
        let none = self.int(0);
        let made = self.help(Helper::ListNew, &[element, none])?;
        self.open_scope();
        self.bind_native(&kept, Ty::List(item), Some(made), true);
        let append = Expr {
            kind: ExprKind::Attr {
                object: Box::new(name_expr(&kept, span)),
                name: "append".to_owned(),
                optional: false,
            },
            span,
        };
        let arg = Arg { name: None, is_mut: false, value: name_expr(param, span) };
        let call = Expr { kind: ExprKind::Call { callee: Box::new(append), args: vec![arg] }, span };
        let names = [param.to_owned()];
        let each = comp_loop(&names, &name_expr(name, span), Some(test), StmtKind::Expr(call), span);
        self.stmt_inside_expr(&each)?;
        let made = self.local(&kept).and_then(|local| local.var).map(|var| self.b.use_var(var));
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.remove(&kept);
        }
        self.close_scope()?;
        made.ok_or_else(|| super::internal("a lost list", span))
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
        (var, start, count): (&str, Value, Value),
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
