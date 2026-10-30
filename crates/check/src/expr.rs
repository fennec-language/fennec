//! Checking expressions and working out their types: names, operators, fields, indexing,
//! comprehensions, arrow functions, and `?`, `?.` and `else`.

use fennec_syntax::{BinOp, CompKind, Diagnostic, Expr, ExprKind, FStrPart, Span, UnaryOp};

use crate::builtins;
use crate::function::FnCx;
use crate::infer::Hole;
use crate::messages::{describe, might_be_none, unhandled_result, unknown_name};
use crate::program::TypeKind;
use crate::stmt::{not_none, path};
use crate::types::{Type, a, article};

impl FnCx<'_> {
    /// An expression's type, recorded for the typed tree.
    pub(crate) fn expr(&mut self, expr: &Expr) -> Type {
        let ty = self.expr_kind(expr);
        let ty = self.checker.settled(&ty);
        self.record(expr.span, &ty);
        ty
    }

    fn expr_kind(&mut self, expr: &Expr) -> Type {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Str(_) => Type::Str,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::None => Type::None,
            ExprKind::FStr(_)
            | ExprKind::List(_)
            | ExprKind::Set(_)
            | ExprKind::Tuple(_)
            | ExprKind::Dict(_)
            | ExprKind::Comprehension { .. } => self.written_out(expr),
            ExprKind::Name(name) => self.name(name, span),
            ExprKind::Variant(name) => self.construct_variant(None, name, &[], span),
            ExprKind::Unary { op, expr: inner } => self.unary(*op, inner, span),
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                self.condition(lhs, op.symbol());
                self.track(lhs);
                let before = self.snapshot();
                self.narrow(&not_none(lhs, *op == BinOp::And));
                self.condition(rhs, op.symbol());
                self.restore(&before);
                Type::Bool
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let (left, right) = (self.expr(lhs), self.expr(rhs));
                self.binary(*op, &left, &right, span, (lhs.span, rhs.span))
            }
            ExprKind::Cond { then, cond, otherwise } => {
                self.condition(cond, "if");
                self.track(cond);
                let before = self.snapshot();
                self.narrow(&not_none(cond, true));
                let then = self.expr(then);
                self.restore(&before);
                self.narrow(&not_none(cond, false));
                let otherwise = self.expr(otherwise);
                self.restore(&before);
                self.join_or_report(&[then, otherwise], span, "the two sides of this `if` give")
            }
            ExprKind::Else { value, default } => {
                let value = self.expr(value);
                let default = self.expr(default);
                match value {
                    Type::Optional(inner) | Type::Result(inner, _) => {
                        self.join_or_report(&[*inner, default], span, "the two sides of this `else` give")
                    }
                    Type::None => default,
                    other => other,
                }
            }
            ExprKind::Try { value, context } => self.try_value(value, context.as_deref()),
            ExprKind::Call { callee, args } => self.call(callee, args, span),
            ExprKind::Attr { object, name, optional } => self.attr(object, name, *optional, span),
            ExprKind::Index { object, index } => self.index(object, index, span),
            ExprKind::Slice { object, start, end } => {
                self.slice(object, start.as_deref(), end.as_deref(), span)
            }
            ExprKind::Lambda { params, body } => self.lambda(params, body, &[], span),
            ExprKind::Match { subject, arms } => self.match_expr(subject, arms, span),
        }
    }

    /// An f-string, or a collection written out or built by a comprehension.
    fn written_out(&mut self, expr: &Expr) -> Type {
        let span = expr.span;
        match &expr.kind {
            ExprKind::FStr(parts) => {
                for part in parts {
                    if let FStrPart::Expr { expr, .. } = part {
                        self.expr(expr);
                    }
                }
                Type::Str
            }
            ExprKind::List(items) => {
                Type::list(self.join_items(items, span, Hole::EmptyList, "this list mixes"))
            }
            ExprKind::Set(items) => {
                Type::Set(Box::new(self.join_items(items, span, Hole::EmptySet, "this set mixes")))
            }
            ExprKind::Tuple(items) => Type::Tuple(items.iter().map(|item| self.expr(item)).collect()),
            ExprKind::Dict(pairs) => {
                let (keys, values): (Vec<Expr>, Vec<Expr>) = pairs.iter().cloned().unzip();
                let key = self.join_items(&keys, span, Hole::EmptyDict, "this dict's keys mix");
                let value = self.join_items(&values, span, Hole::EmptyDict, "this dict's values mix");
                Type::Dict(Box::new(key), Box::new(value))
            }
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                let parts = (elem.as_ref(), value.as_deref(), cond.as_deref());
                let (elem, value) = self.comprehension(parts, names, iter, span);
                match kind {
                    CompKind::List | CompKind::Generator => Type::list(elem),
                    CompKind::Set => Type::Set(Box::new(elem)),
                    CompKind::Dict => Type::Dict(Box::new(elem), Box::new(value)),
                }
            }
            _ => Type::Unknown,
        }
    }

    /// The type of the items of a list, set or dict written out: what they all fit, or a
    /// type still to be worked out when there are none.
    fn join_items(&mut self, items: &[Expr], span: Span, hole: Hole, what: &str) -> Type {
        let types: Vec<Type> = items.iter().map(|item| self.expr(item)).collect();
        if types.is_empty() {
            return self.checker.fresh(span, hole);
        }
        self.join_or_report(&types, span, what)
    }

    /// What values from several places all fit, or `Unknown` after reporting that they
    /// don't: "this list mixes an int and a str".
    pub(crate) fn join_or_report(&mut self, types: &[Type], span: Span, what: &str) -> Type {
        if let Some(joined) = self.checker.join_all(types) {
            return joined;
        }
        let mut kinds: Vec<String> = Vec::new();
        for ty in types {
            let kind = a(&self.checker.settled(ty));
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        let help = if kinds.iter().all(|kind| kind == "an int" || kind == "a float") {
            "ints and floats don't mix: write whole floats with a decimal point, like `10.0`, or convert with float(...)"
        } else {
            "a value has one type: convert these to one, or make an enum with a case for each"
        };
        self.error(Diagnostic::new(format!("{what} {}", kinds.join(" and ")), span).with_help(help));
        Type::Unknown
    }

    fn name(&mut self, name: &str, span: Span) -> Type {
        if let Some((ty, _)) = self.lookup(name) {
            return ty;
        }
        if self.checker.program.functions.contains_key(name) {
            return Type::Function(name.to_owned());
        }
        if name == "PI" {
            return Type::Float;
        }
        let problem = if self.checker.program.is_type_name(name) {
            Diagnostic::new(format!("`{name}` is a type, not a value"), span)
                .with_help(format!("make one with `{name}(...)`"))
        } else if builtins::is_builtin(name) {
            Diagnostic::new(format!("`{name}` can only be called"), span)
                .with_help(format!("call it, like `{name}(...)`, or pass an arrow function that calls it"))
        } else {
            unknown_name(name, span)
        };
        self.error(problem);
        Type::Unknown
    }

    /// A name used as a type, like `Enemy` in `Enemy.at(...)`, rather than as a value.
    pub(crate) fn as_type_name<'e>(&self, expr: &'e Expr) -> Option<&'e str> {
        match &expr.kind {
            ExprKind::Name(name)
                if self.lookup(name).is_none() && self.checker.program.is_type_name(name) =>
            {
                Some(name)
            }
            _ => None,
        }
    }

    /// An operator on two values. `spans` are the two sides', for pointing at one of them.
    pub(crate) fn binary(
        &mut self,
        op: BinOp,
        left: &Type,
        right: &Type,
        span: Span,
        spans: (Span, Span),
    ) -> Type {
        let (left, right) = self.settle_sides(left, right);
        match op {
            BinOp::Eq | BinOp::NotEq => {
                self.comparable(&left, &right, op, span);
                return Type::Bool;
            }
            BinOp::In | BinOp::NotIn => {
                self.contains(&left, &right, span);
                return Type::Bool;
            }
            BinOp::Is | BinOp::IsNot | BinOp::And | BinOp::Or => return Type::Bool,
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                if self.usable_operands(&left, &right, span, spans) {
                    self.ordered(&left, &right, op, span, spans.1);
                }
                return Type::Bool;
            }
            _ => {}
        }
        if !self.usable_operands(&left, &right, span, spans) {
            return Type::Unknown;
        }
        if let (Type::Named(name), Some(method)) = (&left, op.method_name())
            && let Some((key, _)) = self.checker.program.method(name, method)
        {
            return self.call_operator(&key, &right, spans.1);
        }
        match (&left, &right) {
            (Type::Int, Type::Int) if op == BinOp::Div => Type::Float,
            (Type::Int, Type::Int) => Type::Int,
            (Type::Float, Type::Float) => Type::Float,
            (Type::Str, Type::Str) if op == BinOp::Add => Type::Str,
            (Type::List(a), Type::List(b)) if op == BinOp::Add => {
                let item = self.join_or_report(&[*a.clone(), *b.clone()], span, "these lists hold");
                Type::list(item)
            }
            (Type::Unknown, _) | (_, Type::Unknown) => Type::Unknown,
            (Type::Int, Type::Float) | (Type::Float, Type::Int) => {
                self.error(
                    Diagnostic::new(format!("can't mix int and float in `{}`", op.symbol()), span)
                        .with_help("convert one side with float(...) or int(...)"),
                );
                Type::Unknown
            }
            _ => {
                let message = format!(
                    "can't use `{}` on {} and {}",
                    op.symbol(),
                    article(&left.kind()),
                    article(&right.kind())
                );
                self.error(Diagnostic::new(message, span));
                Type::Unknown
            }
        }
    }

    /// Both sides of an operator, settled. A side still being worked out takes the other
    /// side's type when that's a plain value, as in `items[0] + 1`.
    fn settle_sides(&mut self, left: &Type, right: &Type) -> (Type, Type) {
        let (left, right) = (self.checker.settled(left), self.checker.settled(right));
        if let (Type::Var(_), other) | (other, Type::Var(_)) = (&left, &right)
            && matches!(other, Type::Int | Type::Float | Type::Str | Type::Bool)
        {
            self.checker.fits(&left, &right);
        }
        (self.checker.settled(&left), self.checker.settled(&right))
    }

    /// `==` and `!=` need two values that could be equal.
    fn comparable(&mut self, left: &Type, right: &Type, op: BinOp, span: Span) {
        if self.checker.fits(left, right) || self.checker.fits(right, left) {
            return;
        }
        let diagnostic = Diagnostic::new(
            format!("`{}` compares {} with {}, which are never equal", op.symbol(), a(left), a(right)),
            span,
        );
        self.error(match (left, right) {
            (Type::Int, Type::Float) | (Type::Float, Type::Int) => {
                diagnostic.with_help("convert one side with float(...) or int(...)")
            }
            _ => diagnostic.with_help("compare values of the same type"),
        });
    }

    /// `x in items`: `x` has to be the kind of thing `items` holds.
    fn contains(&mut self, item: &Type, collection: &Type, span: Span) {
        let held = match collection {
            Type::List(held) | Type::Set(held) | Type::Dict(held, _) => held.as_ref().clone(),
            Type::IdMap(held) => Type::Id(held.clone()),
            Type::Str => Type::Str,
            Type::Unknown | Type::Var(_) => return,
            other => {
                let message = format!("`in` looks inside a collection, but this is {}", a(other));
                self.error(Diagnostic::new(message, span));
                return;
            }
        };
        if !self.checker.fits(&held, item) {
            let message = format!("can't look for {} in {}", a(item), a(collection));
            self.error(Diagnostic::new(message, span));
        }
    }

    /// `<`, `<=`, `>` and `>=` need two values of a type with an order.
    fn ordered(&mut self, left: &Type, right: &Type, op: BinOp, span: Span, right_span: Span) {
        if let Type::Named(name) = left
            && let Some((key, _)) = self.checker.program.method(name, "<")
        {
            self.call_operator(&key, right, right_span);
            return;
        }
        let orderable =
            |ty: &Type| matches!(ty, Type::Int | Type::Float | Type::Str | Type::Tuple(_) | Type::List(_));
        match (left, right) {
            (Type::Unknown, _) | (_, Type::Unknown) => {}
            (Type::Int, Type::Float) | (Type::Float, Type::Int) => self.error(
                Diagnostic::new(format!("can't mix int and float in `{}`", op.symbol()), span)
                    .with_help("convert one side with float(...) or int(...)"),
            ),
            _ if left == right && orderable(left) => {}
            _ => {
                let message = format!("can't order {} and {} with `{}`", a(left), a(right), op.symbol());
                let help = format!("define `def <(self, other: ...)` to order {}", describe(left));
                self.error(Diagnostic::new(message, span).with_help(help));
            }
        }
    }

    /// Checks neither side of an operator is an unhandled result or might be None.
    fn usable_operands(&mut self, left: &Type, right: &Type, span: Span, spans: (Span, Span)) -> bool {
        if matches!(left, Type::Result(..)) || matches!(right, Type::Result(..)) {
            self.error(unhandled_result(span));
            return false;
        }
        self.usable(left, spans.0) && self.usable(right, spans.1)
    }

    /// Checks a value isn't an unhandled result and can't be None.
    pub(crate) fn usable(&mut self, ty: &Type, span: Span) -> bool {
        let problem = match ty {
            Type::Result(..) => unhandled_result(span),
            ty if ty.maybe_none() => might_be_none(span),
            _ => return true,
        };
        self.error(problem);
        false
    }

    fn unary(&mut self, op: UnaryOp, inner: &Expr, span: Span) -> Type {
        let ty = self.expr(inner);
        match (op, &ty) {
            (_, Type::Unknown) | (UnaryOp::Not, Type::Bool) | (UnaryOp::Neg, Type::Int | Type::Float) => ty,
            (UnaryOp::Not, _) => {
                self.error(
                    Diagnostic::new(format!("`not` needs a bool, but this is {}", article(&ty.kind())), span)
                        .with_help("Fennec has no truthiness: compare explicitly, like `len(items) == 0`"),
                );
                Type::Bool
            }
            (UnaryOp::Neg, Type::Result(..)) => {
                self.error(unhandled_result(inner.span));
                Type::Unknown
            }
            (UnaryOp::Neg, ty) if ty.maybe_none() => {
                self.error(might_be_none(inner.span));
                Type::Unknown
            }
            (UnaryOp::Neg, _) => {
                self.error(Diagnostic::new(format!("can't negate {}", article(&ty.kind())), span));
                Type::Unknown
            }
        }
    }

    fn attr(&mut self, object: &Expr, name: &str, optional: bool, span: Span) -> Type {
        if let Some(type_name) = self.as_type_name(object) {
            return self.type_attr(type_name, name, span);
        }
        // A field path that's been checked for None, like `player.weapon`.
        if !optional
            && let Some(path) = path(object)
            && let Some((ty, _)) = self.lookup(&format!("{path}.{name}"))
        {
            return ty;
        }
        let ty = self.expr(object);
        self.field_of(&ty, name, optional, span)
    }

    pub(crate) fn field_of(&mut self, ty: &Type, name: &str, optional: bool, span: Span) -> Type {
        match ty {
            Type::Unknown => Type::Unknown,
            Type::None if optional => Type::None,
            Type::Optional(inner) if optional => Type::optional(self.field_of(inner, name, false, span)),
            ty if ty.maybe_none() => {
                self.error(might_be_none(span));
                Type::Unknown
            }
            Type::Result(..) => {
                self.error(unhandled_result(span));
                Type::Unknown
            }
            Type::Named(type_name) => match self.checker.program.type_info(type_name).map(|info| info.kind) {
                Some(TypeKind::Enum) => {
                    self.error(
                        Diagnostic::new(
                            format!("`{type_name}` is an enum, so its fields depend on its case"),
                            span,
                        )
                        .with_help("get them with `match`, like `case .Circle(radius):`"),
                    );
                    Type::Unknown
                }
                Some(_) => {
                    let has_field = self
                        .checker
                        .program
                        .type_info(type_name)
                        .is_some_and(|info| info.def.fields.iter().any(|field| field.name == name));
                    if has_field {
                        return self.checker.field_type(type_name, None, name);
                    }
                    self.error(
                        Diagnostic::new(format!("`{type_name}` has no field `{name}`"), span)
                            .with_help("call a method with parentheses, like `x.length()`"),
                    );
                    Type::Unknown
                }
                None => self.no_field(ty, name, span),
            },
            _ => self.no_field(ty, name, span),
        }
    }

    fn no_field(&mut self, ty: &Type, name: &str, span: Span) -> Type {
        self.error(Diagnostic::new(format!("{} has no field `{name}`", describe(ty)), span));
        Type::Unknown
    }

    /// `Shape.Empty`, or a named constructor that takes nothing, like `Vec3.zero`.
    fn type_attr(&mut self, type_name: &str, name: &str, span: Span) -> Type {
        let info = self.checker.program.type_info(type_name);
        if info.is_some_and(|info| {
            info.kind == TypeKind::Enum && info.def.cases.iter().any(|case| case.name == name)
        }) {
            return self.construct_variant(Some(type_name), name, &[], span);
        }
        let constructor = self
            .checker
            .program
            .method(type_name, name)
            .filter(|(_, entry)| entry.def.params.is_empty())
            .map(|(key, _)| key);
        if let Some(key) = constructor {
            return self.checker.signature(&key).map_or(Type::Unknown, |signature| signature.returns);
        }
        let diagnostic = Diagnostic::new(format!("`{type_name}` has nothing called `{name}`"), span);
        let takes_args = self.checker.program.method(type_name, name).is_some();
        self.error(if takes_args {
            diagnostic.with_help(format!("call it with its arguments, like `{type_name}.{name}(...)`"))
        } else {
            diagnostic
        });
        Type::Unknown
    }

    fn index(&mut self, object: &Expr, index: &Expr, span: Span) -> Type {
        if let Some(type_name) = self.as_type_name(object) {
            self.error(
                Diagnostic::new(format!("`{type_name}[...]` is a type, not a value"), span)
                    .with_help(format!("call it to make one, like `{type_name}[Enemy]()`")),
            );
            return Type::Unknown;
        }
        let ty = self.expr(object);
        let at = self.expr(index);
        match &ty {
            Type::Unknown | Type::Var(_) => Type::Unknown,
            Type::Result(..) => {
                self.error(unhandled_result(span));
                Type::Unknown
            }
            ty if ty.maybe_none() => {
                self.error(might_be_none(span));
                Type::Unknown
            }
            Type::List(item) => {
                self.key_fits(&Type::Int, &at, index.span);
                item.as_ref().clone()
            }
            Type::Str => {
                self.key_fits(&Type::Int, &at, index.span);
                Type::Str
            }
            Type::Tuple(items) => self.tuple_item(items, index),
            Type::Dict(key, item) => {
                self.key_fits(key, &at, index.span);
                item.as_ref().clone()
            }
            Type::IdMap(item) => {
                self.key_fits(&Type::Id(item.clone()), &at, index.span);
                item.as_ref().clone()
            }
            Type::Named(name) => {
                if let Some((key, _)) = self.checker.program.method(name, "[]") {
                    return self.call_operator(&key, &at, index.span);
                }
                self.error(
                    Diagnostic::new(format!("`{name}` can't be indexed"), span)
                        .with_help("define `def [](self, index: ...)` to index it"),
                );
                Type::Unknown
            }
            other => {
                self.error(Diagnostic::new(format!("{} can't be indexed", article(&other.kind())), span));
                Type::Unknown
            }
        }
    }

    /// Checks an index or key fits what's indexed: an int for a list, a key for a dict.
    fn key_fits(&mut self, expected: &Type, actual: &Type, span: Span) {
        if self.checker.fits(expected, actual) {
            return;
        }
        let message = match expected {
            Type::Int => format!("an index must be an int, not {}", article(&actual.kind())),
            other => format!("this is looked up by {}, not {}", a(other), a(actual)),
        };
        self.error(Diagnostic::new(message, span));
    }

    /// `pair[0]`: the item's type. The index has to be written as a number, unless every
    /// item has the same type.
    fn tuple_item(&mut self, items: &[Type], index: &Expr) -> Type {
        let at = match index.kind {
            ExprKind::Int(at) => usize::try_from(at).ok(),
            _ => None,
        };
        if let Some(item) = at.and_then(|at| items.get(at)) {
            return item.clone();
        }
        if at.is_some() {
            let message =
                format!("this tuple has {} items, so there's no item {}", items.len(), at.unwrap_or(0));
            self.error(Diagnostic::new(message, index.span));
            return Type::Unknown;
        }
        if let Some(joined) = self.checker.join_all(items) {
            return joined;
        }
        self.error(
            Diagnostic::new(
                "a tuple whose items have different types needs its index written as a number",
                index.span,
            )
            .with_help("write it like `pair[0]`, or unpack it: `first, second = pair`"),
        );
        Type::Unknown
    }

    fn slice(&mut self, object: &Expr, start: Option<&Expr>, end: Option<&Expr>, span: Span) -> Type {
        let ty = self.expr(object);
        for bound in start.into_iter().chain(end) {
            let at = self.expr(bound);
            if !self.checker.fits(&Type::Int, &at) {
                let message = format!("a slice position must be an int, not {}", article(&at.kind()));
                self.error(Diagnostic::new(message, bound.span));
            }
        }
        match ty {
            Type::List(_) | Type::Str | Type::Unknown => ty,
            Type::Tuple(items) => self.tuple_slice(&items, start, end, span),
            other => {
                self.error(Diagnostic::new(format!("{} can't be sliced", article(&other.kind())), span));
                Type::Unknown
            }
        }
    }

    /// `pair[1:]`: a shorter tuple. Its bounds have to be written as numbers, so its items'
    /// types are known.
    fn tuple_slice(&mut self, items: &[Type], start: Option<&Expr>, end: Option<&Expr>, span: Span) -> Type {
        let bound = |at: Option<&Expr>, default: usize| match at.map(|at| &at.kind) {
            None => Some(default),
            Some(ExprKind::Int(n)) => usize::try_from(*n).ok().map(|n| n.min(items.len())),
            Some(_) => None,
        };
        if let (Some(from), Some(to)) = (bound(start, 0), bound(end, items.len())) {
            return Type::Tuple(items.get(from..to.max(from)).unwrap_or_default().to_vec());
        }
        self.error(
            Diagnostic::new("a tuple's slice needs its positions written as numbers", span)
                .with_help("write it like `triple[1:]`"),
        );
        Type::Unknown
    }

    /// The type of each item when looping over a value.
    pub(crate) fn item_type(&mut self, ty: &Type, span: Span) -> Type {
        match self.checker.settled(ty) {
            Type::List(item) | Type::Set(item) | Type::IdMap(item) | Type::Dict(item, _) => *item,
            Type::Tuple(items) => {
                if let Some(joined) = self.checker.join_all(&items) {
                    return joined;
                }
                self.error(
                    Diagnostic::new("can't loop over a tuple whose items have different types", span)
                        .with_help("unpack it instead: `first, second = pair`"),
                );
                Type::Unknown
            }
            Type::Str => Type::Str,
            Type::Unknown | Type::Var(_) => Type::Unknown,
            Type::Result(..) => {
                self.error(unhandled_result(span));
                Type::Unknown
            }
            ty if ty.maybe_none() => {
                self.error(might_be_none(span));
                Type::Unknown
            }
            Type::Named(name) => self.iterated(&name, span),
            other => {
                self.error(Diagnostic::new(format!("can't loop over {}", article(&other.kind())), span));
                Type::Unknown
            }
        }
    }

    /// The items of a type looped over with its own `def iter`.
    fn iterated(&mut self, name: &str, span: Span) -> Type {
        if let Some((key, _)) = self.checker.program.method(name, "iter") {
            return match self.checker.signature(&key).map(|signature| signature.returns) {
                Some(Type::List(item)) => *item,
                Some(Type::Unknown) | None => Type::Unknown,
                Some(other) => {
                    let message =
                        format!("`{name}.iter` gives back {}, so it can't be looped over", a(&other));
                    self.error(Diagnostic::new(message, span).with_help("make `iter` yield its items"));
                    Type::Unknown
                }
            };
        }
        self.error(
            Diagnostic::new(format!("can't loop over a `{name}`"), span)
                .with_help("define `def iter(self)` that yields its items"),
        );
        Type::Unknown
    }

    /// A comprehension's element and, for a dict, value types. `parts` are the element,
    /// the value for a dict, and the condition.
    fn comprehension(
        &mut self,
        parts: (&Expr, Option<&Expr>, Option<&Expr>),
        names: &[String],
        iter: &Expr,
        span: Span,
    ) -> (Type, Type) {
        let (elem, value, cond) = parts;
        let iter_ty = self.expr(iter);
        let item = self.item_type(&iter_ty, iter.span);
        let before = self.snapshot();
        self.push(false);
        self.bind_names(names, &item, false, (iter.span, span));
        if let Some(cond) = cond {
            self.condition(cond, "if");
            self.track(cond);
            self.narrow(&not_none(cond, true));
        }
        let elem = self.expr(elem);
        let value = value.map_or(Type::Unit, |value| self.expr(value));
        self.pop();
        self.restore(&before);
        (elem, value)
    }

    /// An arrow function. Its parameters' types come from where it's passed, so it has to
    /// be passed straight to something that says what it receives.
    pub(crate) fn lambda(&mut self, params: &[String], body: &Expr, types: &[Type], span: Span) -> Type {
        self.push(true);
        let mut known = Vec::new();
        for (at, param) in params.iter().enumerate() {
            let given = types.get(at).cloned();
            let ty = given.clone().unwrap_or(Type::Unknown);
            // A type that's unknown because of a mistake already reported says nothing new.
            if given.is_none() {
                self.error(
                    Diagnostic::new(format!("can't tell the type of `{param}`"), span).with_help(
                        "pass the arrow function straight to a call that gives it values, like `sorted(items, key=x => ...)`, or write a nested `def` with its parameters' types",
                    ),
                );
            }
            self.declare(param, ty.clone(), true, span);
            known.push(ty);
        }
        let returns = self.expr(body);
        self.pop();
        let ty = Type::Arrow(known, Box::new(returns));
        self.record(span, &ty);
        ty
    }

    /// `value?`: the value inside a result, passing its error up.
    fn try_value(&mut self, value: &Expr, context: Option<&Expr>) -> Type {
        let ty = self.expr(value);
        let inner = match &ty {
            Type::Result(inner, error) => {
                if context.is_none() {
                    self.errors.push(error.as_ref().clone());
                }
                inner.as_ref().clone()
            }
            Type::Unknown => Type::Unknown,
            other => {
                let help = if other.maybe_none() {
                    "`?` passes errors up, not None: give a default with `else`, or check with `if x is None:`"
                } else {
                    "only something that can fail gives a result to pass up"
                };
                let message = format!("`?` needs a result, but this is {}", a(other));
                self.error(Diagnostic::new(message, value.span).with_help(help));
                Type::Unknown
            }
        };
        if let Some(context) = context {
            let error = self.expr(context);
            if error != Type::Unknown && !self.checker.is_error_type(&error) {
                let message = format!("`from` needs an error, but this is {}", a(&error));
                self.error(Diagnostic::new(message, context.span));
            }
            self.errors.push(error);
        }
        inner
    }
}
