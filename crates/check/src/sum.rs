//! `sum`: numbers need nothing, and any other type with a `+` passes a start, because only
//! the program knows what its zero is.

use fennec_syntax::{Arg, BinOp, Diagnostic, ExprKind, Span};

use crate::function::FnCx;
use crate::generic::show;
use crate::messages::mismatch;
use crate::program::TypeKind;
use crate::types::Type;

/// A call to `sum`, for checking its items once their type is known.
pub(crate) struct SumCall {
    item: Type,
    has_start: bool,
    /// The first argument as it's written, if it's short enough to repeat in a message.
    items: String,
    span: Span,
}

impl FnCx<'_> {
    pub(crate) fn sum(&mut self, args: &[Arg], plain: &[(Type, Span)], span: Span) -> Type {
        let Some((items, at)) = plain.first() else { return Type::Unknown };
        let item = self.item_type(items, *at);
        let start = plain.get(1);
        let start_fits = start.is_none_or(|(start, _)| self.checker.fits(&item, start));
        let written = match args.first().map(|arg| &arg.value.kind) {
            Some(ExprKind::Name(name)) => name.clone(),
            _ => "...".to_owned(),
        };
        let call = SumCall { item: item.clone(), has_start: start.is_some(), items: written, span };
        if self.checker.settled(&item).is_known() {
            self.check_sum(&call);
        } else {
            self.pending_sums.push(call);
        }
        if let (Some((start, at)), false) = (start, start_fits) {
            let expected = self.checker.settled(&item);
            self.error(mismatch("`sum`'s start", &expected, start, *at));
        }
        item
    }

    /// Checks the sums left until the end of the function's check.
    pub(crate) fn check_pending_sums(&mut self) {
        for call in std::mem::take(&mut self.pending_sums) {
            if self.checker.settled(&call.item).is_known() {
                self.check_sum(&call);
            }
        }
    }

    fn check_sum(&mut self, call: &SumCall) {
        let item = self.checker.settled(&call.item);
        if !self.sum_adds(&item, call.span) || call.has_start {
            return;
        }
        match item {
            Type::Int => {}
            // What runs a program doesn't know types, so it's told an empty list starts at 0.0.
            Type::Float => {
                self.checker.float_sums.insert((call.span.start, call.span.end));
            }
            other => {
                let start = self.start_hint(&other);
                let message =
                    format!("`sum` of `{other}` needs a starting value: sum({}, {start})", call.items);
                self.error(Diagnostic::new(message, call.span));
            }
        }
    }

    /// Whether the items can be added with `+`, which has to give back another item.
    fn sum_adds(&mut self, item: &Type, span: Span) -> bool {
        let has_plus = match item {
            Type::Int | Type::Float | Type::Str | Type::Bytes | Type::List(_) => true,
            Type::Param(name) => self.bound_method(name, "+").is_some(),
            other => {
                other.method_owner().is_some_and(|owner| self.checker.program.method(owner, "+").is_some())
            }
        };
        if !has_plus {
            self.error(no_plus(item, span));
            return false;
        }
        let total = self.binary(BinOp::Add, item, item, span, (span, span));
        if self.checker.fits(item, &total) {
            return true;
        }
        let message =
            format!("`sum` needs `+` to give back {}, but `{item}`'s gives {}", show(item), show(&total));
        self.error(Diagnostic::new(message, span));
        false
    }

    /// A start to suggest. Whether zero is the right value is for the program to say.
    fn start_hint(&mut self, item: &Type) -> String {
        let Some(name) = item.type_name().map(str::to_owned) else {
            let start = match item {
                Type::Str => "\"\"",
                Type::List(_) => "[]",
                _ => "...",
            };
            return start.to_owned();
        };
        let Some(info) = self.checker.program.type_info(&name) else { return "...".to_owned() };
        if info.kind != TypeKind::Struct {
            return "...".to_owned();
        }
        let required: Vec<String> = info
            .def
            .fields
            .iter()
            .filter(|field| field.default.is_none())
            .map(|field| field.name.clone())
            .collect();
        let fields: Vec<String> = required
            .iter()
            .map(|field| {
                let zero = match self.checker.field_type(&name, None, field) {
                    Type::Int => "0",
                    Type::Float => "0.0",
                    Type::Str => "\"\"",
                    _ => "...",
                };
                format!("{field}={zero}")
            })
            .collect();
        format!("{name}({})", fields.join(", "))
    }
}

fn no_plus(item: &Type, span: Span) -> Diagnostic {
    let written = format!("`{item}`");
    match item {
        Type::Param(_) => Diagnostic::new(format!("`sum` adds with `+`, but {written} has no `def +`"), span)
            .with_help(format!("give {written} an interface with `def +(self, other: Self) -> Self`")),
        other if other.type_name().is_some() => {
            Diagnostic::new(format!("`sum` adds with `+`, but {written} has no `def +`"), span)
                .with_help(format!("add `def +(self, other: {item}) -> {item}` to {written}"))
        }
        _ => Diagnostic::new(format!("`sum` adds with `+`, but {written} has no `+`"), span),
    }
}
