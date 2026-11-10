//! The type expected where an expression is written: a parameter's, a field's, the return
//! type, a name's declared type, or the other side of `==`. It decides which enum a
//! leading-dot case belongs to when more than one enum has a case by that name:
//!
//! ```fennec
//! def literal(tok: Tok) -> ExprKind:
//!     match tok:
//!         case .Int(value): .Int(value)   # the return type makes it ExprKind.Int
//! ```

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span};

use crate::function::FnCx;
use crate::program::TypeKind;
use crate::types::{Type, a};

impl FnCx<'_> {
    /// An expression's type, where a value of type `expected` goes.
    pub(crate) fn expr_as(&mut self, expr: &Expr, expected: &Type) -> Type {
        self.expecting = Some(expected.clone());
        self.expr(expr)
    }

    /// An expression's type, given what's expected of it. Only leading-dot cases, `match`es and
    /// lists use what's expected. A list written where a list of optionals goes is one of those,
    /// since it can come to hold `None`.
    pub(crate) fn expected_kind(&mut self, expr: &Expr, expected: &Type) -> Type {
        self.note_named_case(expr, expected);
        if let Some(ty) = self.sized_literal(expr, expected) {
            return ty;
        }
        if let Some(ty) = self.sized_collection(expr, expected) {
            return ty;
        }
        if let Some(ty) = self.union_literal(expr, expected) {
            return ty;
        }
        if matches!(self.checker.settled(expected).without_none(), Type::LiteralStr)
            && let Some(ty) = self.literal_str(expr)
        {
            return ty;
        }
        match &expr.kind {
            ExprKind::Variant(case) => {
                let owner = self.case_owner(case, expected, expr.span);
                self.construct_variant(owner.as_deref(), case, &[], expr.span)
            }
            ExprKind::Call { callee, args } if let ExprKind::Variant(case) = &callee.kind => {
                let owner = self.case_owner(case, expected, callee.span);
                self.construct_variant(owner.as_deref(), case, args, expr.span)
            }
            ExprKind::Match { subject, arms } => {
                self.match_expr(subject, arms, expr.span, Some(expected.clone()))
            }
            // `Box(value=1)` where a `Box[int | str]` goes: a generic type built where one of it
            // is expected takes its types from there, so each field is checked against them.
            ExprKind::Call { callee, args }
                if let ExprKind::Name(name) = &callee.kind
                    && let Some(types) = self.expected_generics(name, expected) =>
            {
                self.construct_with(name, (args, &types), expr.span)
            }
            // `step: (int) -> int = n => n + 1`: an arrow takes its parameters' types from the
            // function type expected of it.
            ExprKind::Lambda { params, body }
                if let Type::Arrow(types, _) = self.checker.settled(expected).without_none() =>
            {
                self.lambda(params, body, &types, expr.span)
            }
            ExprKind::List(items)
                if let Type::Array(item, size) = self.checker.settled(expected).without_none() =>
            {
                self.array_literal(items, &item, size, expr.span)
            }
            // `[array[int, 3](), [1, 2, 3]]` where a list of arrays goes: each item is written
            // where an array goes, so a list literal among them is one.
            ExprKind::List(items)
                if let Type::List(item) = self.checker.settled(expected).without_none()
                    && self.checker.settled(&item).without_none().kind() == "array" =>
            {
                for each in items {
                    self.item_as(each, &item);
                }
                Type::List(item)
            }
            ExprKind::Dict(pairs)
                if let Type::Dict(key, value) = self.checker.settled(expected).without_none()
                    && self.checker.settled(&value).without_none().kind() == "array" =>
            {
                for (each_key, each_value) in pairs {
                    self.item_as(each_key, &key);
                    self.item_as(each_value, &value);
                }
                Type::Dict(key, value)
            }
            ExprKind::List(_) => {
                let ty = self.expr_kind(expr);
                let wanted = self.checker.settled(expected);
                let of_optionals = matches!(&wanted, Type::List(item) if matches!(&**item, Type::Optional(held)
                    if matches!(**held, Type::Int | Type::Float | Type::Bool | Type::Str | Type::Named(_))));
                if of_optionals && self.checker.fits(&wanted, &ty) { wanted } else { ty }
            }
            _ => self.expr_kind(expr),
        }
    }

    /// A list, set, dict or tuple literal written where one holding sized numbers goes: each
    /// item goes where its sized number does, so it takes that size and is checked to fit, as in
    /// `bytes: list[u8] = [1, 2, 255]` and `pair: (u8, int) = (3, 1)`. `None` for anything else.
    fn sized_collection(&mut self, expr: &Expr, expected: &Type) -> Option<Type> {
        let wanted = self.checker.settled(expected).without_none();
        if !holds_sized(&wanted) {
            return None;
        }
        match (&expr.kind, wanted) {
            (ExprKind::List(items), Type::List(item)) => {
                for each in items {
                    self.item_as(each, &item);
                }
                Some(Type::List(item))
            }
            (ExprKind::Set(items), Type::Set(item)) => {
                for each in items {
                    self.item_as(each, &item);
                }
                Some(Type::Set(item))
            }
            (ExprKind::Dict(pairs), Type::Dict(key, value)) => {
                for (each_key, each_value) in pairs {
                    self.item_as(each_key, &key);
                    self.item_as(each_value, &value);
                }
                Some(Type::Dict(key, value))
            }
            (ExprKind::Tuple(items), Type::Tuple(types)) if items.len() == types.len() => {
                for (each, ty) in items.iter().zip(&types) {
                    self.item_as(each, ty);
                }
                Some(Type::Tuple(types))
            }
            _ => None,
        }
    }

    /// A number literal written where a sized number goes takes that size, checked to fit:
    /// `pixel: u8 = 255`, `speed: f32 = 1.5`. `None` for anything else.
    fn sized_literal(&mut self, expr: &Expr, expected: &Type) -> Option<Type> {
        let Type::Sized(num) = self.checker.settled(expected).without_none() else { return None };
        let (negated, value) = match &expr.kind {
            ExprKind::Unary { op: fennec_syntax::UnaryOp::Neg, expr: inner } => (true, &inner.kind),
            kind => (false, kind),
        };
        let ty = Type::Sized(num);
        match (value, num.range()) {
            (ExprKind::Int(written), Some((least, most))) => {
                let value = if negated { -i128::from(*written) } else { i128::from(*written) };
                if value < least || value > most {
                    let message = format!("{value} doesn't fit in {ty}");
                    let help = format!("{} holds {least} to {most}", a(&ty));
                    self.error(Diagnostic::new(message, expr.span).with_help(help));
                }
            }
            (ExprKind::Float(written), None) => {
                if written.is_finite() && written.abs() > f64::from(f32::MAX) {
                    let message = format!("{written} is too big for an f32");
                    let help = "an f32 holds up to about 3.4e38; use `float` for more";
                    self.error(Diagnostic::new(message, expr.span).with_help(help));
                }
            }
            _ => return None,
        }
        if negated && let ExprKind::Unary { expr: inner, .. } = &expr.kind {
            self.record(inner.span, &ty);
        }
        Some(ty)
    }

    /// The two sides of an operator. A number literal on one side, opposite a sized number,
    /// takes its size, as in `small + 1` or `2 * small`.
    pub(crate) fn operands(&mut self, lhs: &Expr, rhs: &Expr) -> (Type, Type) {
        if number_literal(lhs) && !number_literal(rhs) {
            let right = self.expr(rhs);
            let left =
                if matches!(right, Type::Sized(_)) { self.expr_as(lhs, &right) } else { self.expr(lhs) };
            return (left, right);
        }
        let left = self.expr(lhs);
        let right = if matches!(left, Type::Sized(_)) && number_literal(rhs) {
            self.expr_as(rhs, &left)
        } else {
            self.expr(rhs)
        };
        (left, right)
    }

    /// What's written where a `LiteralStr` goes, if it's one by how it's written: a string
    /// literal, literals joined with `+`, or `a if c else b` of them. Anything else, like a name typed `LiteralStr`, is
    /// one by its type.
    fn literal_str(&mut self, expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Str(_) => Some(Type::LiteralStr),
            ExprKind::Binary { op: op @ fennec_syntax::BinOp::Add, lhs, rhs } => {
                let left = self.expr_as(lhs, &Type::LiteralStr);
                let right = self.expr_as(rhs, &Type::LiteralStr);
                if matches!((&left, &right), (Type::LiteralStr, Type::LiteralStr)) {
                    return Some(Type::LiteralStr);
                }
                let sides = (left.without_literal(), right.without_literal());
                Some(self.binary(*op, &sides.0, &sides.1, expr.span, (lhs.span, rhs.span)))
            }
            ExprKind::Cond { then, cond, otherwise } => {
                Some(self.cond_value((then, cond, otherwise), Some(&Type::LiteralStr), expr.span))
            }
            _ => None,
        }
    }

    /// The types for a generic struct's parameters, where `name` builds one and `expected` is
    /// one of the same struct, like `Box[int | str]` where `Box(value=1)` goes.
    fn expected_generics(&mut self, name: &str, expected: &Type) -> Option<Vec<Type>> {
        if self.lookup(name).is_some() || self.checker.program.functions.contains_key(name) {
            return None;
        }
        let Type::Generic(wanted, types) = self.checker.settled(expected).without_none() else {
            return None;
        };
        let info = self.checker.program.type_info(name)?;
        let fits = wanted == name
            && info.kind == TypeKind::Struct
            && info.def.cases.is_empty()
            && info.def.generics.len() == types.len();
        fits.then_some(types)
    }

    /// A list, set or dict written where one holding a union goes: each item is checked as a
    /// member, as `[Circle(...), Square(...)]` is where a `list[Shape]` goes. So is one holding
    /// a generic type, so `[Box(value=1)]` where a `list[Box[int | str]]` goes builds each
    /// item as one.
    fn union_literal(&mut self, expr: &Expr, expected: &Type) -> Option<Type> {
        let union = |checker: &mut crate::checker::Checker, ty: &Type| {
            matches!(
                checker.settled(ty).without_none(),
                Type::Union(_) | Type::Generic(..) | Type::LiteralStr
            )
        };
        match (&expr.kind, self.checker.settled(expected).without_none()) {
            (ExprKind::List(items), Type::List(item)) if union(self.checker, &item) => {
                for each in items {
                    self.item_as(each, &item);
                }
                Some(Type::List(item))
            }
            (ExprKind::Set(items), Type::Set(item)) if union(self.checker, &item) => {
                for each in items {
                    self.item_as(each, &item);
                }
                Some(Type::Set(item))
            }
            (ExprKind::Dict(pairs), Type::Dict(key, value))
                if union(self.checker, &key) || union(self.checker, &value) =>
            {
                for (each_key, each_value) in pairs {
                    self.item_as(each_key, &key);
                    self.item_as(each_value, &value);
                }
                Some(Type::Dict(key, value))
            }
            _ => None,
        }
    }

    /// The two sides of `==` or `!=`. A leading-dot case on one side takes its enum from
    /// the other side.
    pub(crate) fn compared(&mut self, lhs: &Expr, rhs: &Expr) -> (Type, Type) {
        if leading_dot(lhs) && !leading_dot(rhs) {
            let right = self.expr(rhs);
            (self.expr_as(lhs, &right), right)
        } else if number_literal(lhs) && !number_literal(rhs) {
            // `0 == pixel`: a number literal takes a sized number's size, and nothing else's.
            let right = self.expr(rhs);
            let left =
                if matches!(right, Type::Sized(_)) { self.expr_as(lhs, &right) } else { self.expr(lhs) };
            (left, right)
        } else {
            let left = self.expr(lhs);
            let right = self.expr_as(rhs, &left);
            (left, right)
        }
    }

    /// Notes an enum's case written with the enum's name where `expected` says it already, which
    /// a leading dot would mean the same as: `expected_kind` gives it this same expected type.
    fn note_named_case(&mut self, expr: &Expr, expected: &Type) {
        let Some((object, case)) = named_case(expr) else { return };
        // An enum a kit has written in for a leading dot covers no source, so there it is one.
        if object.span.start == object.span.end {
            return;
        }
        let Some(written) = self.type_named_by(object) else { return };
        let wanted = match self.checker.settled(expected) {
            Type::Optional(inner) => *inner,
            wanted => wanted,
        };
        if matches!(&wanted, Type::Named(name) if name == written) && self.has_case(written, case) {
            // Code the checker writes, like a generated `serialize`, has its spans in the prelude,
            // and a program rewritten after it checked was held to this as written.
            if object.span.in_prelude() || self.checker.again {
                return;
            }
            let shown = match &object.kind {
                ExprKind::Name(name) => name.as_str(),
                _ => written,
            };
            let message = format!("the type here is known, so write `.{case}` without `{shown}`");
            self.error(
                Diagnostic::new(message, object.span)
                    .with_help("`fennec fix` makes this change")
                    .with_fix(object.span, ""),
            );
        }
    }

    fn has_case(&self, owner: &str, case: &str) -> bool {
        let info = self.checker.program.type_info(owner);
        info.is_some_and(|info| info.def.cases.iter().any(|written| written.name == case))
    }

    /// The type a bare name means, not one reached through a module, which a dot leaves out too.
    fn type_named_by<'e>(&self, object: &'e Expr) -> Option<&'e str> {
        self.as_type_name(object).filter(|_| matches!(object.kind, ExprKind::Name(_)))
    }

    /// The enum or error a leading-dot case belongs to, when the expected type is the error, or
    /// is one of the enums when more than one has a case by that name. Without one, the case is
    /// looked up by its name alone, among the enums.
    fn case_owner(&mut self, case: &str, expected: &Type, span: Span) -> Option<String> {
        let owner = match self.checker.settled(expected) {
            Type::Named(name) => name,
            // `fail .Bad(text)` in a function that `fails ParseError | RangeError`: the one of
            // them with that case.
            Type::Errors(names) => {
                let having: Vec<String> =
                    names.into_iter().filter(|name| self.has_case(name, case)).collect();
                let [owner] = having.as_slice() else { return None };
                self.checker.cases.insert((span.start, span.end), owner.clone());
                return Some(owner.clone());
            }
            Type::Optional(inner) => match *inner {
                Type::Named(name) => name,
                _ => return None,
            },
            _ => return None,
        };
        let is_error =
            self.checker.program.type_info(&owner).is_some_and(|info| info.kind == TypeKind::Error);
        let owners = self.checker.program.enums_with_case(case);
        let chosen =
            if is_error { self.has_case(&owner, case) } else { owners.len() > 1 && owners.contains(&owner) };
        chosen.then(|| {
            self.checker.cases.insert((span.start, span.end), owner.clone());
            owner
        })
    }

    /// One item of a literal where `item` is expected, reporting one that doesn't fit.
    fn item_as(&mut self, each: &Expr, item: &Type) {
        let got = self.expr_as(each, item);
        if !self.checker.fits(item, &got) {
            let message = format!("this holds {}, not {}", a(item), a(&got));
            let mut diagnostic = Diagnostic::new(message, each.span);
            if matches!(item, Type::LiteralStr) {
                diagnostic = diagnostic.with_help(crate::messages::LITERAL_HELP);
            }
            self.error(diagnostic);
        }
    }

    /// `[1.0, 0.0, 0.0, 1.0]` where an `array[float, 4]` goes: every item fits, and there are
    /// exactly as many as the array holds.
    fn array_literal(&mut self, items: &[Expr], item: &Type, size: u64, span: Span) -> Type {
        for each in items {
            let got = self.expr_as(each, item);
            if !self.checker.fits(item, &got) {
                let message = format!("this array holds {}, not {}", a(item), a(&got));
                self.error(Diagnostic::new(message, each.span));
            }
        }
        let written = u64::try_from(items.len()).unwrap_or(u64::MAX);
        if written != size {
            let plural = if written == 1 { "item" } else { "items" };
            self.error(
                Diagnostic::new(
                    format!("this array holds {size} items, but this list has {written} {plural}"),
                    span,
                )
                .with_help(format!(
                    "write all {size}, or make it with `{}()` and set the ones you need",
                    Type::Array(Box::new(item.clone()), size)
                )),
            );
        }
        Type::Array(Box::new(item.clone()), size)
    }
}

/// What's before the dot and after it, in `Shape.Circle` or `Shape.Circle(radius=1.0)`.
fn named_case(expr: &Expr) -> Option<(&Expr, &str)> {
    let at = match &expr.kind {
        ExprKind::Call { callee, .. } => callee,
        _ => expr,
    };
    match &at.kind {
        ExprKind::Attr { object, name, optional: false } => Some((object, name)),
        _ => None,
    }
}

/// Whether an expression is a leading-dot case: `.Circle` or `.Circle(radius=1.0)`.
fn leading_dot(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Variant(_) => true,
        ExprKind::Call { callee, .. } => matches!(callee.kind, ExprKind::Variant(_)),
        _ => false,
    }
}

/// Whether this is a number written out, or one negated: what takes a size from where it goes.
pub(crate) fn number_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) | ExprKind::Float(_) => true,
        ExprKind::Unary { op: fennec_syntax::UnaryOp::Neg, expr: inner } => {
            matches!(inner.kind, ExprKind::Int(_) | ExprKind::Float(_))
        }
        _ => false,
    }
}

/// Whether a type has a sized number in it, as an item, a key, a value or a tuple's part.
fn holds_sized(ty: &Type) -> bool {
    match ty {
        Type::Sized(_) => true,
        Type::List(item) | Type::Set(item) => holds_sized(item),
        Type::Dict(key, value) => holds_sized(key) || holds_sized(value),
        Type::Tuple(items) => items.iter().any(holds_sized),
        _ => false,
    }
}
