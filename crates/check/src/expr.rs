//! Checking expressions and working out their types: names, operators, fields, indexing,
//! comprehensions, arrow functions, and `?`, `?.` and `else`.

use fennec_syntax::{BinOp, CompKind, Diagnostic, Expr, ExprKind, FStrPart, Span, UnaryOp};

use crate::builtins;
use crate::function::FnCx;
use crate::messages::{describe, might_be_none, unhandled_result, unknown_name};
use crate::program::TypeKind;
use crate::stmt::{not_none, path};
use crate::types::{Type, article, join, join_all};

/// What `module.name` means, when `module` is an imported module.
pub(crate) enum Member {
    /// The name to look up instead, like `json.loads` or `Path`.
    Name(String),
    /// The module has nothing by that name, which has been reported.
    Missing,
}

impl FnCx<'_> {
    pub(crate) fn expr(&mut self, expr: &Expr) -> Type {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Str(_) => Type::Str,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::None => Type::None,
            ExprKind::FStr(parts) => {
                for part in parts {
                    if let FStrPart::Expr { expr, .. } = part {
                        self.expr(expr);
                    }
                }
                Type::Str
            }
            ExprKind::Name(name) => self.name(name, span),
            ExprKind::Variant(name) => self.construct_variant(None, name, &[], span),
            ExprKind::List(items) => Type::list(self.join_items(items)),
            ExprKind::Set(items) => Type::Set(Box::new(self.join_items(items))),
            ExprKind::Tuple(items) => Type::Tuple(items.iter().map(|item| self.expr(item)).collect()),
            ExprKind::Dict(pairs) => {
                let (keys, values): (Vec<Type>, Vec<Type>) =
                    pairs.iter().map(|(key, value)| (self.expr(key), self.expr(value))).unzip();
                Type::Dict(Box::new(join_all(&keys)), Box::new(join_all(&values)))
            }
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                let (elem, value) = self.comprehension(elem, value.as_deref(), names, iter, cond.as_deref());
                match kind {
                    CompKind::List | CompKind::Generator => Type::list(elem),
                    CompKind::Set => Type::Set(Box::new(elem)),
                    CompKind::Dict => Type::Dict(Box::new(elem), Box::new(value)),
                }
            }
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
                join(&then, &otherwise)
            }
            ExprKind::Else { value, default } => {
                let value = self.expr(value);
                let default = self.expr(default);
                match value {
                    Type::Optional(inner) | Type::Result(inner, _) => join(&inner, &default),
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
            ExprKind::Lambda { params, body } => self.lambda(params, body, &[]),
            ExprKind::Match { subject, arms } => self.match_expr(subject, arms),
        }
    }

    fn join_items(&mut self, items: &[Expr]) -> Type {
        let types: Vec<Type> = items.iter().map(|item| self.expr(item)).collect();
        join_all(&types)
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
        if let Some(module) = self.checker.program.modules.get(name) {
            self.error(fennec_syntax::module_as_value(name, module, span));
            return Type::Unknown;
        }
        let modules = &self.checker.program.modules;
        if let Some((path, module)) =
            modules.iter().find(|(path, _)| path.split_once('.').is_some_and(|(package, _)| package == name))
        {
            let problem = fennec_syntax::module_as_value(path, module, span);
            self.error(problem);
            return Type::Unknown;
        }
        if modules.contains_key(&format!("std.{name}")) {
            self.error(fennec_syntax::short_module(name, span));
            return Type::Unknown;
        }
        let imported = self.checker.program.imported(name, span).is_some();
        if !self.checker.program.is_type_name(name) && !builtins::is_builtin(name) && !imported {
            self.error(unknown_name(name, span));
        }
        Type::Unknown
    }

    /// For `json.loads` where `json` is an imported module, the name it means: a module
    /// function like `json.loads`, or a type like `Path`. A name the module doesn't have is
    /// reported here.
    pub(crate) fn module_member(&mut self, object: &Expr, name: &str, span: Span) -> Option<Member> {
        let alias = fennec_syntax::dotted(object)?;
        if self.lookup(alias.split('.').next().unwrap_or_default()).is_some() {
            return None;
        }
        let modules = &self.checker.program.modules;
        let Some(&module) = modules.get(&alias) else {
            // `std.toml` after `import std.json`, which binds `std` but not `std.toml`.
            let path = format!("{alias}.{name}");
            let package = format!("{alias}.");
            if !modules.keys().any(|bound| bound.starts_with(&package)) {
                return None;
            }
            let problem = fennec_syntax::package_member(&path, modules.get(&path).copied(), span);
            self.error(problem);
            return Some(Member::Missing);
        };
        if let Err(problem) = fennec_syntax::std_member(module, name, span) {
            self.error(problem);
            return Some(Member::Missing);
        }
        let qualified = format!("{module}.{name}");
        Some(Member::Name(if builtins::is_builtin(&qualified) { qualified } else { name.to_owned() }))
    }

    /// A name used as a type, like `Enemy` in `Enemy.at(...)` or `game.IdMap` in
    /// `game.IdMap[Enemy]()`, rather than as a value.
    pub(crate) fn as_type_name<'e>(&self, expr: &'e Expr) -> Option<&'e str> {
        let name = match &expr.kind {
            ExprKind::Name(name) => name,
            ExprKind::Attr { object, name, optional: false } => {
                let alias = fennec_syntax::dotted(object)?;
                if self.lookup(alias.split('.').next().unwrap_or_default()).is_some() {
                    return None;
                }
                let module = self.checker.program.modules.get(&alias)?;
                fennec_syntax::std_member(module, name, expr.span).ok()?;
                name
            }
            _ => return None,
        };
        (self.lookup(name).is_none() && self.checker.program.is_type_name(name)).then_some(name.as_str())
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
        match op {
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Is
            | BinOp::IsNot
            | BinOp::In
            | BinOp::NotIn
            | BinOp::And
            | BinOp::Or => {
                return Type::Bool;
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                self.usable_operands(left, right, span, spans);
                return Type::Bool;
            }
            _ => {}
        }
        if !self.usable_operands(left, right, span, spans) {
            return Type::Unknown;
        }
        if let (Type::Named(name), Some(method)) = (left, op.method_name())
            && let Some((key, _)) = self.checker.program.method(name, method)
        {
            return self.call_operator(&key, right, spans.1);
        }
        match (left, right) {
            (Type::Int, Type::Int) if op == BinOp::Div => Type::Float,
            (Type::Int, Type::Int) => Type::Int,
            (Type::Float, Type::Float) => Type::Float,
            (Type::Str, Type::Str) if op == BinOp::Add => Type::Str,
            (Type::List(a), Type::List(b)) if op == BinOp::Add => Type::list(join(a, b)),
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
        match self.module_member(object, name, span) {
            Some(Member::Name(member)) => return self.name(&member, span),
            Some(Member::Missing) => return Type::Unknown,
            None => {}
        }
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
                Some(TypeKind::Enum) => Type::Unknown,
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
        self.error(Diagnostic::new(format!("`{type_name}` has nothing called `{name}`"), span));
        Type::Unknown
    }

    fn index(&mut self, object: &Expr, index: &Expr, span: Span) -> Type {
        if self.as_type_name(object).is_some() {
            return Type::Unknown;
        }
        let ty = self.expr(object);
        let at = self.expr(index);
        let int_index = |cx: &mut Self| {
            if !matches!(at, Type::Int | Type::Unknown) {
                cx.error(Diagnostic::new(
                    format!("an index must be an int, not {}", article(&at.kind())),
                    index.span,
                ));
            }
        };
        match &ty {
            Type::Unknown => Type::Unknown,
            Type::Result(..) => {
                self.error(unhandled_result(span));
                Type::Unknown
            }
            ty if ty.maybe_none() => {
                self.error(might_be_none(span));
                Type::Unknown
            }
            Type::List(item) => {
                int_index(self);
                item.as_ref().clone()
            }
            Type::Str => {
                int_index(self);
                Type::Str
            }
            Type::Tuple(items) => tuple_item(items, index),
            Type::Dict(_, item) | Type::IdMap(item) => item.as_ref().clone(),
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

    fn slice(&mut self, object: &Expr, start: Option<&Expr>, end: Option<&Expr>, span: Span) -> Type {
        let ty = self.expr(object);
        for bound in start.into_iter().chain(end) {
            let at = self.expr(bound);
            if !matches!(at, Type::Int | Type::Unknown) {
                let message = format!("a slice position must be an int, not {}", article(&at.kind()));
                self.error(Diagnostic::new(message, bound.span));
            }
        }
        match ty {
            Type::List(_) | Type::Str | Type::Unknown => ty,
            Type::Tuple(_) => Type::Unknown,
            other => {
                self.error(Diagnostic::new(format!("{} can't be sliced", article(&other.kind())), span));
                Type::Unknown
            }
        }
    }

    /// The type of each item when looping over a value.
    pub(crate) fn item_type(&mut self, ty: &Type, span: Span) -> Type {
        match ty {
            Type::List(item) | Type::Set(item) | Type::IdMap(item) | Type::Dict(item, _) => {
                item.as_ref().clone()
            }
            Type::Tuple(items) => join_all(items),
            Type::Str => Type::Str,
            Type::Unknown => Type::Unknown,
            Type::Result(..) => {
                self.error(unhandled_result(span));
                Type::Unknown
            }
            ty if ty.maybe_none() => {
                self.error(might_be_none(span));
                Type::Unknown
            }
            Type::Named(name) => {
                if let Some((key, _)) = self.checker.program.method(name, "iter") {
                    return match self.checker.signature(&key).map(|signature| signature.returns) {
                        Some(Type::List(item)) => *item,
                        _ => Type::Unknown,
                    };
                }
                self.error(
                    Diagnostic::new(format!("can't loop over a `{name}`"), span)
                        .with_help("define `def iter(self)` that yields its items"),
                );
                Type::Unknown
            }
            other => {
                self.error(Diagnostic::new(format!("can't loop over {}", article(&other.kind())), span));
                Type::Unknown
            }
        }
    }

    /// A comprehension's element and, for a dict, value types.
    fn comprehension(
        &mut self,
        elem: &Expr,
        value: Option<&Expr>,
        names: &[String],
        iter: &Expr,
        cond: Option<&Expr>,
    ) -> (Type, Type) {
        let iter_ty = self.expr(iter);
        let item = self.item_type(&iter_ty, iter.span);
        let before = self.snapshot();
        self.push(false);
        self.bind_names(names, &item, false, iter.span);
        if let Some(cond) = cond {
            self.condition(cond, "if");
            self.track(cond);
            self.narrow(&not_none(cond, true));
        }
        let elem = self.expr(elem);
        let value = value.map_or(Type::Unknown, |value| self.expr(value));
        self.pop();
        self.restore(&before);
        (elem, value)
    }

    /// An arrow function. Its parameters' types come from where it's passed, when known.
    pub(crate) fn lambda(&mut self, params: &[String], body: &Expr, types: &[Type]) -> Type {
        self.push(true);
        for (at, param) in params.iter().enumerate() {
            self.declare(param, types.get(at).cloned().unwrap_or(Type::Unknown), true);
        }
        self.expr(body);
        self.pop();
        Type::Unknown
    }

    /// `value?`: the value inside a result or optional, passing an error or None up.
    fn try_value(&mut self, value: &Expr, context: Option<&Expr>) -> Type {
        let ty = self.expr(value);
        match (context, &ty) {
            (Some(context), _) => {
                let error = self.expr(context);
                self.errors.push(error);
            }
            (None, Type::Result(_, error)) => self.errors.push(error.as_ref().clone()),
            _ => {}
        }
        match ty {
            Type::Result(value, _) => *value,
            other => other.without_none(),
        }
    }
}

/// `pair[0]`: the item's type when the index is written as a number.
fn tuple_item(items: &[Type], index: &Expr) -> Type {
    let at = match index.kind {
        ExprKind::Int(at) => usize::try_from(at).ok(),
        _ => None,
    };
    match at.and_then(|at| items.get(at)) {
        Some(item) => item.clone(),
        None => join_all(items),
    }
}
