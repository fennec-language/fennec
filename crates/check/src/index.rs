//! Indexing and slicing: lists and strings by position, dicts by key, `IdMap`s by `Id`,
//! tuples by a position written as a number, and types with their own `def []`.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span, UnaryOp};

use crate::function::FnCx;
use crate::messages::{might_be_none, unhandled_result};
use crate::types::{Type, a, article};

impl FnCx<'_> {
    pub(crate) fn index(&mut self, object: &Expr, index: &Expr, span: Span) -> Type {
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
            Type::Named(name) | Type::Generic(name, _) => {
                if let Some((key, _)) = self.checker.program.method(name, "[]") {
                    return self.call_operator(&ty, &key, &at, index.span);
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

    pub(crate) fn slice(
        &mut self,
        object: &Expr,
        start: Option<&Expr>,
        end: Option<&Expr>,
        span: Span,
    ) -> Type {
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
        let len = i64::try_from(items.len()).unwrap_or(i64::MAX);
        // A bound below 0 counts from the end, as in Python.
        let bound = |at: Option<&Expr>, default: usize| {
            let written = match at.map(|at| &at.kind) {
                None => return Some(default),
                Some(ExprKind::Int(n)) => *n,
                Some(ExprKind::Unary { op: UnaryOp::Neg, expr }) => match expr.kind {
                    ExprKind::Int(n) => n.checked_neg()?,
                    _ => return None,
                },
                Some(_) => return None,
            };
            let from_start = if written < 0 { written.saturating_add(len).max(0) } else { written.min(len) };
            usize::try_from(from_start).ok()
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
}
