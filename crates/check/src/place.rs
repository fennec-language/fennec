//! Assignment, and places: a name, or a path into one, like `world.player.health` or
//! `scores[2]`. Changing a place needs its name to be `mut`.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Span, TypeExpr};

use crate::function::{Binding, FnCx, Origin};
use crate::messages::mismatch;
use crate::types::{Type, a};

/// The name a place starts at: `world` in `world.player.health`.
pub(crate) fn root(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. } => root(object),
        _ => None,
    }
}

impl FnCx<'_> {
    /// `ref unit = units[at]`: a name for a field or item of something that can change, for
    /// the rest of the block. Its type is the place's.
    pub(crate) fn local_ref(&mut self, name: &str, place: &Expr, at: Span) {
        let stepped = matches!(place.kind, ExprKind::Attr { optional: false, .. } | ExprKind::Index { .. });
        let what = "a `ref` names a field or an item, like `ref unit = units[at]`";
        if root(place) == Some(name) {
            let message = format!("`{name}` is already the name this `ref` reaches through");
            self.error(Diagnostic::new(message, at).with_help("give the `ref` a name of its own"));
        } else if stepped && root(place).is_some() {
            self.mut_place(place, what);
        } else {
            let help = match &place.kind {
                ExprKind::Name(held) => format!("`{held}` is a name already: use it as it is"),
                _ => "name what's held in a mut name, like `ref unit = units[at]`".to_owned(),
            };
            self.error(Diagnostic::new(what, place.span).with_help(help));
        }
        let ty = self.expr(place);
        self.declare(name, ty, true, at);
    }

    /// Checks an expression names a place that can change. `what` explains what's wrong
    /// when it isn't a place at all.
    pub(crate) fn mut_place(&mut self, expr: &Expr, what: &str) {
        if !matches!(expr.kind, ExprKind::Name(_))
            && let Some(handle) = self.through_shared(expr)
        {
            let message = format!("`{handle}` is `shared`, so change it in one of its methods");
            self.error(Diagnostic::new(message, expr.span).with_help(format!(
                "a `shared` value's methods take its lock: add a `ref self` method that makes the change, or lock it for a block with `with {handle} as ref locked:`"
            )));
            return;
        }
        match root(expr) {
            None => self.error(Diagnostic::new(what, expr.span)),
            Some(name) => {
                self.forget_fields(name);
                if self.changes_constant(name, expr.span) {
                    return;
                }
                if let Some((_, false)) = self.lookup(name) {
                    let mut problem = self.cannot_change(name, expr.span);
                    if let Some(fix) = self.write_it_there(name, expr) {
                        // `mut target` would change a copy, so the change would be lost.
                        problem.hint = None;
                        problem = problem.with_help(fix);
                    }
                    self.error(problem);
                }
            }
        }
    }

    /// Reports a place inside a `shared T` reached outside a `lock`, like `scores.best = {}`,
    /// saying whether it was one.
    fn shared_place(&mut self, place: &Expr) -> bool {
        let (ExprKind::Attr { object, .. } | ExprKind::Index { object, .. }) = &place.kind else {
            return false;
        };
        if self.shared_place(object) {
            return true;
        }
        match self.declared_type_of(object) {
            Some(ty) if crate::lock::held(&ty).is_some() => self.outside_lock(object, &ty, place.span),
            _ => false,
        }
    }

    /// How to change what `target = units[at]` read from, when `target.health` can't change.
    fn write_it_there(&self, name: &str, expr: &Expr) -> Option<String> {
        if self.origin(name) != Some(Origin::Local) {
            return None;
        }
        let source = self.read_from.get(name)?;
        let rest = crate::loops::place_text(expr);
        let holder = source.split(['[', '.']).next().unwrap_or(source);
        Some(format!(
            "to change `{holder}`: write to `{source}{}`",
            rest.strip_prefix(name).unwrap_or_default()
        ))
    }

    pub(crate) fn assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
        written: Option<&TypeExpr>,
    ) {
        match &target.kind {
            ExprKind::Name(name) => {
                if !declare_mut
                    && matches!(value.kind, ExprKind::Index { .. } | ExprKind::Attr { .. })
                    && self.lookup(name).is_none()
                    && root(value).is_some_and(|from| self.lookup(from).is_some())
                {
                    self.read_from.insert(name.clone(), crate::loops::place_text(value));
                }
                let expected = match written {
                    Some(written) => self.written_type(written),
                    None if op.is_none() => self.declared_type(name),
                    // `query += " where id = 1"` keeps a `LiteralStr` one when what's added is.
                    None if op == Some(BinOp::Add) => {
                        self.declared_type(name).filter(|ty| matches!(ty, Type::LiteralStr | Type::Sized(_)))
                    }
                    // `small -= 1`: a number literal takes the size of what it changes.
                    None => self.declared_type(name).filter(|ty| matches!(ty, Type::Sized(_))),
                };
                let ty = match &expected {
                    Some(expected) => self.expr_as(value, expected),
                    None => self.expr(value),
                };
                if name.starts_with(&format!("_{}", fennec_syntax::visit::HIDDEN))
                    && matches!(self.checker.settled(&ty), Type::Result(..))
                {
                    self.error(crate::messages::dropped_failure(value.span));
                }
                match written {
                    Some(written) => {
                        self.declare_written(name, written, &ty, (target.span, value.span), declare_mut);
                    }
                    None => self.assign_name(name, target.span, op, ty, declare_mut),
                }
            }
            ExprKind::Tuple(names) => {
                let ty = self.expr(value);
                let parts = self.unpack(&ty, names.len(), value.span);
                for (name, ty) in names.iter().zip(parts) {
                    if let ExprKind::Name(text) = &name.kind {
                        let new = self.lookup(text).is_none();
                        self.assign_name(text, name.span, None, ty, false);
                        if new {
                            self.unpacked(text);
                        }
                    }
                }
            }
            ExprKind::Index { object, index } if op.is_none() && self.has_set_index(object) => {
                let held = self.set_index(object, index, value);
                self.record(target.span, &held);
            }
            _ if self.shared_place(target) => {
                self.expr(value);
            }
            _ => {
                self.mut_place(target, "this can't be assigned to");
                self.own_indexes_can_write(target);
                let current = self.place_type(target);
                self.record(target.span, &current);
                let adds_literal = op == Some(BinOp::Add) && matches!(current, Type::LiteralStr);
                // `small += 1`: a number literal takes the size of what it changes.
                let sized = matches!(current, Type::Sized(_)) && crate::expect::number_literal(value);
                let mut ty = if op.is_none() || adds_literal || sized {
                    self.expr_as(value, &current)
                } else {
                    self.expr(value)
                };
                if let Some(op) = op {
                    ty = self.binary(op, &current, &ty, target.span, (target.span, value.span));
                }
                if !self.checker.fits(&current, &ty) {
                    let current = self.checker.settled(&current);
                    self.error(mismatch("this", &current, &ty, value.span));
                }
            }
        }
    }

    /// The type a field or item holds, as declared: not narrowed by a check for None.
    fn place_type(&mut self, target: &Expr) -> Type {
        match &target.kind {
            ExprKind::Attr { object, name, optional: false } => {
                let ty = self.expr(object);
                self.field_of(&ty, name, false, target.span)
            }
            _ => self.expr(target),
        }
    }

    /// `name: type = value`, which declares a name with its type written.
    fn declare_written(
        &mut self,
        name: &str,
        written: &TypeExpr,
        ty: &Type,
        spans: (Span, Span),
        is_mut: bool,
    ) {
        let (at, value) = spans;
        if self.changes_constant(name, at) {
            return;
        }
        if self.lookup(name).is_some() {
            self.error(
                Diagnostic::new(format!("`{name}` already exists"), at)
                    .with_help(format!("to change it, write `{name} = ...` without its type")),
            );
            return;
        }
        let declared = self.resolve(written);
        if !self.checker.fits(&declared, ty) {
            self.error(mismatch(&format!("`{name}`"), &declared, ty, value));
        }
        let binding = Binding {
            name: name.to_owned(),
            ty: ty.clone(),
            declared,
            is_mut,
            origin: Origin::Local,
            mut_at: Some(at.start),
        };
        self.declare_with(binding, at);
        self.record(at, &self.checker.settled(ty));
    }

    fn assign_name(&mut self, name: &str, span: Span, op: Option<BinOp>, ty: Type, declare_mut: bool) {
        if self.changes_constant(name, span) {
            return;
        }
        if !declare_mut && self.origin(name) == Some(Origin::Locked) {
            self.error(
                Diagnostic::new(
                    format!("`{name}` names what this `lock` holds, so it can't name anything else"),
                    span,
                )
                .with_help("give the other value a name of its own"),
            );
            return;
        }
        let existing = self.lookup(name);
        match (existing, op) {
            (Some(_), _) if declare_mut => self.error(
                Diagnostic::new(format!("`{name}` already exists"), span)
                    .with_help(format!("to change it, write `{name} = ...` without `mut`")),
            ),
            (None, Some(_)) => self.error(Diagnostic::new(format!("`{name}` doesn't exist yet"), span)),
            (None, None) => self.declare_assigned(name, ty.clone(), declare_mut, span),
            (Some((_, false)), _) => self.error(self.cannot_change(name, span)),
            (Some((current, true)), Some(op)) => {
                let ty = self.binary(op, &current, &ty, span, (span, span));
                self.give(name, ty, span);
            }
            (Some(_), None) => self.give(name, ty.clone(), span),
        }
        let held = self.lookup(name).map_or(ty, |(held, _)| held);
        self.record(span, &held);
        self.forget_fields(name);
    }

    /// Gives an existing name a new value, which has to fit the type it was declared with.
    fn give(&mut self, name: &str, ty: Type, span: Span) {
        let declared = self.declared_type(name).unwrap_or(Type::Unknown);
        if !self.checker.fits(&declared, &ty) {
            let declared = self.checker.settled(&declared);
            self.error(
                Diagnostic::new(
                    format!("`{name}` holds {}, so it can't be given {}", a(&declared), a(&ty)),
                    span,
                )
                .with_help(if matches!(declared, Type::LiteralStr) {
                    crate::messages::LITERAL_HELP.to_owned()
                } else {
                    format!("a name keeps one type: use a new name for {}", a(&ty))
                }),
            );
            return;
        }
        self.set_type(name, ty);
    }

    /// Changing a place through a type's own `def []` writes the item back with its `def []=`,
    /// so the type needs one.
    fn own_indexes_can_write(&mut self, place: &Expr) {
        let (ExprKind::Attr { object, .. } | ExprKind::Index { object, .. }) = &place.kind else { return };
        self.own_indexes_can_write(object);
        if !matches!(place.kind, ExprKind::Index { .. }) {
            return;
        }
        let ty = self.peek_type(object);
        if let Some(name) = ty.type_name().filter(|name| self.checker.program.method(name, "[]").is_some())
            && self.checker.program.method(name, "[]=").is_none()
            && self.checker.program.method(name, "[]mut").is_none()
        {
            let message = format!("`{name}` has a `def []` but no `def []=`, so its items can't be changed");
            let help = format!(
                "define the `ref` form, `def [](ref self, i: int)`, or `def []=(ref self, index: ..., value: ...)` for `{name}`"
            );
            self.error(Diagnostic::new(message, place.span).with_help(help));
        }
    }

    /// Whether `object[...] = value` calls the object's own `def []=`.
    fn has_set_index(&mut self, object: &Expr) -> bool {
        let ty = self.peek_type(object);
        ty.type_name().is_some_and(|name| self.checker.program.method(name, "[]=").is_some())
    }

    /// `grid[at] = value` on a type with its own `def []=`. Returns the type of the value it
    /// takes.
    fn set_index(&mut self, object: &Expr, index: &Expr, value: &Expr) -> Type {
        let ty = self.expr(object);
        let Some(name) = ty.type_name() else { return Type::Unknown };
        let Some((key, _)) = self.checker.program.method(name, "[]=") else { return Type::Unknown };
        let Some(signature) = self.method_signature(&ty, &key) else { return Type::Unknown };
        if signature.params.first().is_some_and(|param| param.is_mut) {
            self.mut_place(object, "`[]=` changes its value, so call it on something held in a mut name");
        }
        let args = [(index, signature.params.get(1)), (value, signature.params.get(2))];
        for (arg, param) in args {
            let actual = self.expr(arg);
            if let Some(param) = param {
                self.check_arg(param, &actual, arg.span);
            }
        }
        signature.params.get(2).map_or(Type::Unknown, |param| param.ty.clone())
    }
}
