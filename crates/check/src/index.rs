//! Indexing and slicing: lists and strings by position, dicts by key, `Table`s by `Id`,
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
            Type::List(item) | Type::Buffer(item) => {
                self.key_fits(&Type::Int, &at, index.span);
                item.as_ref().clone()
            }
            Type::Array(item, size) => {
                self.key_fits(&Type::Int, &at, index.span);
                self.array_index_in_range(index, *size);
                item.as_ref().clone()
            }
            Type::Str => {
                self.key_fits(&Type::Int, &at, index.span);
                Type::Str
            }
            Type::Bytes => {
                self.key_fits(&Type::Int, &at, index.span);
                Type::Int
            }
            Type::Tuple(items) => self.tuple_item(items, index),
            Type::Dict(key, item) => {
                self.key_fits(key, &at, index.span);
                item.as_ref().clone()
            }
            Type::Table(item) => {
                self.key_fits(&Type::Id(item.clone()), &at, index.span);
                item.as_ref().clone()
            }
            Type::Named(name) if name == "Args" => {
                self.key_fits(&Type::Int, &at, index.span);
                Type::Str
            }
            Type::Named(name) | Type::Generic(name, _) => {
                if let Some((key, _)) = self.checker.program.method(name, "[]") {
                    return self.call_operator(&ty, &key, &at, index.span);
                }
                // With only the `mut` form, reaching an item takes a `mut` name.
                if let Some((key, _)) = self.checker.program.method(name, "[]mut") {
                    let what = format!("`{name}` has only the `ref` form of `def []`, so index a mut name");
                    self.mut_place(object, &what);
                    return self.call_operator(&ty, &key, &at, index.span);
                }
                self.error(
                    Diagnostic::new(format!("`{name}` can't be indexed"), span)
                        .with_help("define `def [](self, index: ...)` to index it"),
                );
                Type::Unknown
            }
            other => {
                let help = match other {
                    Type::Set(_) => {
                        "a set has no order: check for an item with `item in items`, or index `sorted(items)`"
                    }
                    Type::Iterator(_) => "make a list of it first, as `list(items)[0]`",
                    _ => "index a list, str, dict, tuple or array, as `items[0]`",
                };
                let message = format!("{} can't be indexed", article(&other.kind()));
                self.error(Diagnostic::new(message, span).with_help(help));
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
        self.error(Diagnostic::new(message, span).with_help(convert_help(expected, actual)));
    }

    /// `slots[20]` on an `array[Item?, 20]`: a position written as a number has to be inside
    /// the array, counting from the end when it's below 0 as for a list.
    fn array_index_in_range(&mut self, index: &Expr, size: u64) {
        let written = match &index.kind {
            ExprKind::Int(n) => *n,
            ExprKind::Unary { op: UnaryOp::Neg, expr } => match expr.kind {
                ExprKind::Int(n) => n.saturating_neg(),
                _ => return,
            },
            _ => return,
        };
        let size_signed = i64::try_from(size).unwrap_or(i64::MAX);
        if written < size_signed && written >= -size_signed {
            return;
        }
        let last = size - 1;
        self.error(
            Diagnostic::new(format!("this array has {size} items, so there's no item {written}"), index.span)
                .with_help(format!("its items are 0 to {last}: write one of those, or `-1` for the last")),
        );
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
            let help = format!("its items are 0 to {}: write one of those", items.len().saturating_sub(1));
            self.error(Diagnostic::new(message, index.span).with_help(help));
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
                self.error(Diagnostic::new(message, bound.span).with_help(convert_help(&Type::Int, &at)));
            }
        }
        match ty {
            Type::List(_) | Type::Str | Type::Bytes | Type::Unknown => ty,
            Type::Tuple(items) => self.tuple_slice(&items, start, end, span),
            other => {
                let help = match other {
                    Type::Array(..) => "make a list of it first, as `list(items)[1:3]`",
                    _ => "slice a list, str, bytes or tuple, as `items[1:3]`",
                };
                let message = format!("{} can't be sliced", article(&other.kind()));
                self.error(Diagnostic::new(message, span).with_help(help));
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

/// How to turn `actual` into the `expected` index or key.
fn convert_help(expected: &Type, actual: &Type) -> String {
    match (expected, actual) {
        (Type::Int, Type::Str | Type::LiteralStr) => {
            "read the text as a number, as `items[int(text)?]`".to_string()
        }
        (Type::Int, Type::Float) => "turn it into an int, as `items[int(position)]`".to_string(),
        (Type::Str, Type::Int | Type::Float | Type::Sized(_)) => {
            "turn it into text, as `table[str(key)]`".to_string()
        }
        (Type::Int, _) => "write an int, as `items[0]`".to_string(),
        (other, _) => format!("look it up by {}", a(other)),
    }
}
