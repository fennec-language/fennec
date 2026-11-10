//! Checking expressions and working out their types: names, operators, fields, indexing,
//! comprehensions, arrow functions, and `?`, `?.` and `else`.

use fennec_syntax::{BinOp, CompKind, Diagnostic, Expr, ExprKind, FStrPart, Span, StmtKind, UnaryOp};

use crate::builtins;
use crate::function::FnCx;
use crate::infer::Hole;
use crate::messages::{bad_spec, describe, might_be_none, unhandled_result, unknown_name};
use crate::program::TypeKind;
use crate::stmt::{not_none, path};
use crate::types::{Type, a, article};

/// What `module.name` means, when `module` is an imported module.
pub(crate) enum Member {
    /// The name to look up instead, like `json.loads` or `Path`.
    Name(String),
    /// The module has nothing by that name, which has been reported.
    Missing,
}

impl FnCx<'_> {
    /// An expression's type, recorded for the typed tree.
    pub(crate) fn expr(&mut self, expr: &Expr) -> Type {
        let expected = self.expecting.take();
        // A `LiteralStr` is one only where one is asked for: used for anything else, like
        // `name = literal`, it's a `str`.
        let literal = expected.as_ref().is_some_and(|expected| self.checker.settled(expected).has_literal());
        let ty = match expected {
            Some(expected) => self.expected_kind(expr, &expected),
            None => self.expr_kind(expr),
        };
        let ty = self.checker.settled(&ty);
        let ty = if literal { ty } else { ty.without_literal() };
        if crate::lend::lent(&ty).is_some()
            && matches!(expr.kind, ExprKind::Call { .. } | ExprKind::Try { .. })
            && !self.with_values.contains(&(expr.span.start, expr.span.end))
        {
            self.source_outside_with(expr, &ty);
        }
        self.record(expr.span, &ty);
        ty
    }

    pub(crate) fn expr_kind(&mut self, expr: &Expr) -> Type {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Duration(_) => Type::named("Duration"),
            ExprKind::Bytes(_) => Type::Bytes,
            ExprKind::Folder(_) => Type::named("EmbeddedFolder"),
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
                self.narrow_members(&crate::stmt::members(lhs, *op == BinOp::And));
                self.condition(rhs, op.symbol());
                self.restore(&before);
                Type::Bool
            }
            ExprKind::Binary { op: op @ (BinOp::Eq | BinOp::NotEq), lhs, rhs } => {
                let (left, right) = self.compared(lhs, rhs);
                if *op == BinOp::NotEq && left == Type::Float {
                    self.not_a_nan_test(lhs, rhs, span);
                }
                self.binary(*op, &left, &right, span, (lhs.span, rhs.span))
            }
            ExprKind::Binary { op: BinOp::Is | BinOp::IsNot, lhs, rhs }
                if let ExprKind::Name(member) = &rhs.kind
                    && self.lookup(member).is_none()
                    && self.member_type(member, rhs.span).is_some() =>
            {
                self.is_member(lhs, member, rhs.span)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let (left, right) = self.operands(lhs, rhs);
                self.binary(*op, &left, &right, span, (lhs.span, rhs.span))
            }
            ExprKind::Cond { then, cond, otherwise } => self.cond_value((then, cond, otherwise), None, span),
            ExprKind::Else { value, default } => self.else_value(value, default, span),
            ExprKind::Try { value, context } => self.try_value(value, context.as_deref(), span),
            ExprKind::Call { callee, args } => self.call(callee, args, span),
            ExprKind::Attr { object, name, optional } => self.read_attr(object, name, *optional, span),
            ExprKind::Index { object, index } => self.index(object, index, span),
            ExprKind::Slice { object, start, end } => {
                self.slice(object, start.as_deref(), end.as_deref(), span)
            }
            ExprKind::Lambda { params, body } => self.lambda(params, body, &[], span),
            ExprKind::Match { subject, arms } => self.match_expr(subject, arms, span, None),
        }
    }

    /// An f-string, or a collection written out or built by a comprehension.
    fn written_out(&mut self, expr: &Expr) -> Type {
        let ty = self.written_out_kind(expr);
        if let Some(problem) = crate::program::optional_inside(&self.checker.settled(&ty), expr.span) {
            self.error(problem);
        }
        ty
    }

    fn written_out_kind(&mut self, expr: &Expr) -> Type {
        let span = expr.span;
        match &expr.kind {
            ExprKind::FStr(parts) => {
                for part in parts {
                    if let FStrPart::Expr { expr, spec } = part {
                        let ty = self.expr(expr);
                        self.not_a_secret(&ty, "this f-string", expr.span);
                        if let Some(problem) = spec.as_deref().and_then(|spec| bad_spec(spec, &ty, expr.span))
                        {
                            self.error(problem);
                        }
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
                let kept =
                    *kind == CompKind::Generator && !self.worked_out_here.contains(&(span.start, span.end));
                self.kept_generators += usize::from(kept);
                let (elem, value) = self.comprehension(parts, names, iter, span, kept);
                self.kept_generators -= usize::from(kept);
                match kind {
                    CompKind::List => Type::list(elem),
                    CompKind::Generator => {
                        let made = Type::Iterator(Box::new(elem));
                        self.checker.generator_types.insert((span.start, span.end), made.to_string());
                        made
                    }
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

    /// A name that doesn't exist, and what was probably meant if there's nothing else to say.
    pub(crate) fn unknown(&self, name: &str, span: Span) -> Diagnostic {
        if let Some(problem) = self.missing_capability(name, span) {
            return problem;
        }
        let problem = unknown_name(name, span);
        match self.similar_name(name).filter(|_| problem.help.is_none()) {
            Some(similar) => problem.with_help(format!("did you mean `{similar}`?")),
            None => problem,
        }
    }

    /// `print` or `console`, which a newcomer writes first: what to write instead, in the function
    /// it's written in. With a `Stdout` already in the signature, it's only a matter of using it.
    fn no_global_output(&self, name: &str, span: Span) -> Diagnostic {
        const WHY: &str = "a function reaches outside itself only through what's in its signature";
        let diagnostic = Diagnostic::new(format!("there's no global `{name}`"), span);
        if let Some(held) = self.name_holding(&Type::named("Stdout")) {
            let diagnostic = diagnostic.with_help(format!("{WHY}: write `{held}.print(...)`"));
            return if name == "print" {
                diagnostic.with_fix(span, format!("{held}.print"))
            } else {
                diagnostic
            };
        }
        let to_add = self.give_it("stdout: Stdout");
        diagnostic.with_help(format!("{WHY}, {to_add}, then `stdout.print(...)`"))
    }

    /// A capability read by its usual name, like `stdout` or `fs`, in a function that wasn't given
    /// it: how to give it one, or what it's called here when it has one under another name.
    fn missing_capability(&self, name: &str, span: Span) -> Option<Diagnostic> {
        let mut letters = name.chars();
        let first = letters.next()?;
        let ty: String = first.to_uppercase().chain(letters).collect();
        if !first.is_lowercase() || !crate::shared::is_capability(&ty) && ty != "Net" {
            return None;
        }
        let diagnostic = Diagnostic::new(format!("`{name}` doesn't exist"), span);
        if let Some(held) = self.name_holding(&Type::named(&ty)) {
            let help = format!("this function's `{ty}` is called `{held}`");
            return Some(diagnostic.with_help(help).with_fix(span, held));
        }
        let to_add = self.give_it(&format!("{name}: {ty}"));
        Some(diagnostic.with_help(format!("{}, {to_add}", crate::threading::NOT_GIVEN)))
    }

    /// How to give the function being checked a parameter, written as `param`: in `main`'s
    /// signature, beside what it already takes, after a test's name, or in a function's
    /// parameters and passed from `main`.
    fn give_it(&self, param: &str) -> String {
        let takes_some =
            self.checker.program.functions.get(&self.key).is_some_and(|main| !main.def.params.is_empty());
        if self.key == "main" && takes_some {
            format!("so add `{param}` to `main`'s parameters")
        } else if self.key == "main" {
            format!("so write `def main({param}):`")
        } else if self.in_a_test() {
            format!("so give this test a `({param})` after its name")
        } else {
            format!("so give `{}` a `{param}` parameter and pass it from `main`", self.key)
        }
    }

    fn name(&mut self, name: &str, span: Span) -> Type {
        if let Some((ty, _)) = self.lookup(name) {
            return ty;
        }
        if self.checker.program.functions.contains_key(name) {
            return Type::Function(name.to_owned());
        }
        if let Some(function) = self.std_function(name, span) {
            return Type::Function(function);
        }
        if let Some(ty) = self.checker.constant_type(name) {
            return ty;
        }
        if name == "PI" {
            return Type::Float;
        }
        // A name an import that failed would have bound was reported at the import.
        if self.checker.program.failed_imports.contains(name) {
            return Type::Unknown;
        }
        let modules = self.checker.program.modules_at(span);
        let problem = if let Some(module) = modules.get(name) {
            fennec_syntax::module_as_value(name, module, span)
        } else if let Some((path, module)) =
            modules.iter().find(|(path, _)| path.split_once('.').is_some_and(|(package, _)| package == name))
        {
            fennec_syntax::module_as_value(path, module, span)
        } else if modules.contains_key(&format!("std.{name}")) {
            fennec_syntax::short_module(name, span)
        } else if self.checker.program.is_type_name(name) {
            Diagnostic::new(format!("`{name}` is a type, not a value"), span)
                .with_help(format!("make one with `{name}(...)`"))
        } else if builtins::is_builtin(name) || self.checker.program.imported(name, span).is_some() {
            Diagnostic::new(format!("`{name}` can only be called"), span)
                .with_help(format!("call it, like `{name}(...)`, or pass an arrow function that calls it"))
        } else if matches!(name, "print" | "console") {
            self.no_global_output(name, span)
        } else {
            self.unknown(name, span)
        };
        self.error(problem);
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
        let modules = self.checker.program.modules_at(span);
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
        if self.checker.program.functions.contains_key(&qualified) {
            self.checker.std_names.insert((span.start, span.end), qualified.clone());
            return Some(Member::Name(qualified));
        }
        Some(Member::Name(if builtins::is_builtin(&qualified) { qualified } else { name.to_owned() }))
    }

    /// The standard library function written in Fennec that a name was imported as.
    pub(crate) fn std_function(&mut self, name: &str, span: Span) -> Option<String> {
        let function = self.checker.program.std_functions.get(name)?.clone();
        self.checker.std_names.insert((span.start, span.end), function.clone());
        Some(function)
    }

    /// A name used as a type, like `Enemy` in `Enemy.at(...)` or `collections.Table` in
    /// `collections.Table[Enemy]()`, rather than as a value.
    pub(crate) fn as_type_name<'e>(&self, expr: &'e Expr) -> Option<&'e str> {
        let name = match &expr.kind {
            ExprKind::Name(name) => name,
            ExprKind::Attr { object, name, optional: false } => {
                let alias = fennec_syntax::dotted(object)?;
                if self.lookup(alias.split('.').next().unwrap_or_default()).is_some() {
                    return None;
                }
                let module = self.checker.program.modules_at(expr.span).get(&alias)?;
                fennec_syntax::std_member(module, name, expr.span).ok()?;
                name
            }
            _ => return None,
        };
        (self.lookup(name).is_none() && self.checker.program.is_type_name(name)).then_some(name.as_str())
    }

    /// `x != x` on a float: how other languages ask about NaN, but here NaN equals NaN, so it's
    /// always False, and a program relying on it would go on as if nothing were wrong.
    fn not_a_nan_test(&mut self, lhs: &Expr, rhs: &Expr, span: Span) {
        let Some(name) =
            fennec_syntax::dotted(lhs).filter(|name| Some(name) == fennec_syntax::dotted(rhs).as_ref())
        else {
            return;
        };
        let message = format!("`{name} != {name}` is always False, since NaN equals NaN here");
        self.error(
            Diagnostic::new(message, span).with_help(format!("write `{name}.is_nan()` to ask about NaN")),
        );
    }

    /// An operator on two values. `spans` are the two sides', for pointing at one of them.
    /// What to do about an int and a float mixed: `//` when the float is an int `/` an int,
    /// which always gives a float, or else convert one side.
    fn mix_help(&self, (left, right): (Span, Span)) -> &'static str {
        let divided = |span: Span| self.int_divisions.contains(&(span.start, span.end));
        if divided(left) || divided(right) {
            "`/` always gives a float, even between ints; `//` divides ints and gives an int"
        } else {
            "convert one side with float(...) or int(...)"
        }
    }

    /// `a if c else b`, each side narrowed by the condition. Where a `LiteralStr` is `expected`,
    /// each side is checked as one, and it's one when both are.
    pub(crate) fn cond_value(
        &mut self,
        (then, cond, otherwise): (&Expr, &Expr, &Expr),
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        self.condition(cond, "if");
        self.track(cond);
        let before = self.snapshot();
        let side = |cx: &mut Self, value: &Expr, holds: bool| {
            cx.narrow(&not_none(cond, holds));
            cx.narrow_members(&crate::stmt::members(cond, holds));
            let ty = match expected {
                Some(expected) => cx.expr_as(value, expected),
                None => cx.expr(value),
            };
            cx.restore(&before);
            ty
        };
        let then_ty = side(self, then, true);
        let otherwise_ty = side(self, otherwise, false);
        if matches!((&then_ty, &otherwise_ty), (Type::LiteralStr, Type::LiteralStr)) {
            return Type::LiteralStr;
        }
        let sides = [then_ty.without_literal(), otherwise_ty.without_literal()];
        if let Some(failing) = self.failing_side((then, otherwise), &sides) {
            self.error(crate::messages::unhandled(failing.span));
            return Type::Unknown;
        }
        self.join_or_report(&sides, span, "the two sides of this `if` give")
    }

    /// The side of a one-line `if` that can fail where the other gives what it would on
    /// success, as in `int(text) if given else 8080`: the two don't join, and the failure is why.
    fn failing_side<'e>(
        &mut self,
        (then, otherwise): (&'e Expr, &'e Expr),
        sides: &[Type; 2],
    ) -> Option<&'e Expr> {
        if self.checker.join_all(sides).is_some() {
            return None;
        }
        let [then_ty, otherwise_ty] = sides;
        [(then, then_ty, otherwise_ty), (otherwise, otherwise_ty, then_ty)].into_iter().find_map(
            |(value, ty, other)| {
                let Type::Result(held, _) = self.checker.settled(ty) else { return None };
                self.checker.join_all(&[*held, other.clone()]).map(|_| value)
            },
        )
    }

    pub(crate) fn binary(
        &mut self,
        op: BinOp,
        left: &Type,
        right: &Type,
        span: Span,
        spans: (Span, Span),
    ) -> Type {
        let (left, right) = self.settle_sides(left, right);
        // Literals joined are a literal; with anything else a `LiteralStr` is a `str`.
        if op == BinOp::Add && matches!((&left, &right), (Type::LiteralStr, Type::LiteralStr)) {
            return Type::LiteralStr;
        }
        let (left, right) = (left.without_literal(), right.without_literal());
        match op {
            BinOp::Eq | BinOp::NotEq => {
                if !self.own_equality(&left, &right, op, span, spans.1) {
                    self.comparable(&left, &right, op, span);
                }
                return Type::Bool;
            }
            BinOp::In | BinOp::NotIn => {
                self.contains(&left, &right, span);
                return Type::Bool;
            }
            BinOp::Is | BinOp::IsNot | BinOp::And | BinOp::Or => return Type::Bool,
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                if self.usable_operands(&left, &right, span, spans) {
                    self.ordered(&left, &right, op, span, spans);
                }
                return Type::Bool;
            }
            _ => {}
        }
        if !self.usable_operands(&left, &right, span, spans) {
            return Type::Unknown;
        }
        if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::ShiftLeft | BinOp::ShiftRight) {
            return self.bits(op, &left, &right, span);
        }
        // A type's own operator, like a struct's `def +` or `str`'s `def *` from std.
        if let (Some(name), Some(method)) = (left.method_owner(), op.method_name())
            && let Some((key, _)) = self.checker.program.method(name, method)
        {
            let key = self.operator_for(&left, key, &right);
            return self.call_operator(&left, &key, &right, spans.1);
        }
        if let Some(method) = op.method_name()
            && let Some(result) = self.interface_operator(&left, method, &right, spans.1)
        {
            return result;
        }
        match (&left, &right) {
            (Type::Int, Type::Int) if op == BinOp::Div => {
                self.int_divisions.insert((span.start, span.end));
                Type::Float
            }
            (Type::Int, Type::Int) => Type::Int,
            (Type::Float, Type::Float) => Type::Float,
            (Type::Sized(num), Type::Sized(other)) if num == other => self.sized_arithmetic(op, &left, span),
            _ if sizes_mixed(&left, &right) => {
                self.error(mixed_sizes(op, &left, &right, span));
                Type::Unknown
            }
            (Type::Str, Type::Str) if op == BinOp::Add => Type::Str,
            (Type::Bytes, Type::Bytes) if op == BinOp::Add => Type::Bytes,
            (Type::List(a), Type::List(b)) if op == BinOp::Add => {
                let item = self.join_or_report(&[*a.clone(), *b.clone()], span, "these lists hold");
                Type::list(item)
            }
            (Type::Unknown, _) | (_, Type::Unknown) => Type::Unknown,
            (Type::Int, Type::Float) | (Type::Float, Type::Int) => {
                let help = self.mix_help(spans);
                self.error(
                    Diagnostic::new(format!("can't mix int and float in `{}`", op.symbol()), span)
                        .with_help(help),
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

    /// An operator between two numbers of one size, which gives that size. `/` always gives a
    /// float, between integers of any size as between ints, and `//` keeps the size.
    fn sized_arithmetic(&mut self, op: BinOp, ty: &Type, span: Span) -> Type {
        if op == BinOp::Div && !matches!(ty, Type::Sized(num) if num.is_float()) {
            self.int_divisions.insert((span.start, span.end));
            return Type::Float;
        }
        ty.clone()
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

    /// `==` and `!=` need two values that could be equal. A value of a type parameter can
    /// be compared only if its bounds say so.
    /// `==` or `!=` on a type with its own `def ==`, which they call. `!=` is written in as
    /// `not (a == b)`. Whether there was one.
    fn own_equality(&mut self, left: &Type, right: &Type, op: BinOp, span: Span, at: Span) -> bool {
        let Some(name) = left.method_owner() else { return false };
        let Some((key, _)) = self.checker.program.method(name, "==") else { return false };
        self.secrets_compared(left, span);
        let returns = self.call_operator(left, &key, right, at);
        if !matches!(returns, Type::Bool | Type::Unknown) {
            let message = format!("`{name}`'s `def ==` gives back {}, so `==` can't use it", a(&returns));
            self.error(Diagnostic::new(message, span).with_help("make `def ==` give back a bool"));
        }
        if op == BinOp::NotEq {
            self.checker.not_equals.insert((span.start, span.end));
        }
        true
    }

    fn comparable(&mut self, left: &Type, right: &Type, op: BinOp, span: Span) {
        if let Type::Param(name) = left
            && let Err((_, fix)) = self.fits_bound(left, "Eq")
        {
            let message = format!("`{name}` could be any type, so `{}` might not work on it", op.symbol());
            self.error(Diagnostic::new(message, span).with_help(fix));
            return;
        }
        let function = |ty: &Type| matches!(ty, Type::Function(_) | Type::Arrow(..));
        if (function(left) && function(right))
            || self.checker.fits(left, right)
            || self.checker.fits(right, left)
        {
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
            _ if sizes_mixed(left, right) => diagnostic.with_help(convert_help(right)),
            _ => diagnostic.with_help("compare values of the same type"),
        });
    }

    /// `&`, `|`, `^`, `<<` and `>>`, which only work on ints, both sides of one size.
    fn bits(&mut self, op: BinOp, left: &Type, right: &Type, span: Span) -> Type {
        if let Type::Sized(num) = left
            && !num.is_float()
        {
            // A shift's amount can be an integer of any size: what's shifted keeps its own.
            let shift = matches!(op, BinOp::ShiftLeft | BinOp::ShiftRight)
                && matches!(right, Type::Int | Type::Sized(_))
                && !matches!(right, Type::Sized(by) if by.is_float());
            if left == right || *right == Type::Unknown || shift {
                return left.clone();
            }
            self.error(mixed_sizes(op, left, right, span));
            return Type::Unknown;
        }
        for side in [left, right] {
            if !matches!(side, Type::Int | Type::Unknown) {
                self.error(not_bits(op, op.symbol(), side, span));
                return Type::Unknown;
            }
        }
        Type::Int
    }

    /// `x in items`: `x` has to be the kind of thing `items` holds.
    fn contains(&mut self, item: &Type, collection: &Type, span: Span) {
        // A type's own `def in`, which `item in value` calls, as `def <` is called for `<`.
        if let Some(name) = collection.type_name()
            && let Some((key, _)) = self.checker.program.method(name, "in")
        {
            let found = self.call_operator(collection, &key, item, span);
            if !matches!(found, Type::Bool | Type::Unknown) {
                let message = format!("`{name}.in` gives back {}, but `in` needs a bool", a(&found));
                self.error(Diagnostic::new(message, span).with_help("make `def in` return `bool`"));
            }
            return;
        }
        let held = match collection {
            Type::List(held)
            | Type::Array(held, _)
            | Type::Buffer(held)
            | Type::Set(held)
            | Type::Dict(held, _) => held.as_ref().clone(),
            Type::Table(held) => Type::Id(held.clone()),
            Type::Str => Type::Str,
            Type::Bytes => Type::Int,
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
    fn ordered(&mut self, left: &Type, right: &Type, op: BinOp, span: Span, spans: (Span, Span)) {
        let right_span = spans.1;
        if let Some(name) = left.type_name()
            && let Some((key, _)) = self.checker.program.method(name, "<")
        {
            self.call_operator(left, &key, right, right_span);
            return;
        }
        if let Type::Param(name) = left
            && !self.is_ordered(left)
        {
            let message = format!("`{name}` could be any type, so it might not have an order");
            let help = format!("write `{name}: Ordered` where `{name}` is declared");
            self.error(Diagnostic::new(message, span).with_help(help));
            return;
        }
        match (left, right) {
            (Type::Unknown, _) | (_, Type::Unknown) => {}
            _ if sizes_mixed(left, right) => self.error(mixed_sizes(op, left, right, span)),
            (Type::Int, Type::Float) | (Type::Float, Type::Int) => {
                let help = self.mix_help(spans);
                self.error(
                    Diagnostic::new(format!("can't mix int and float in `{}`", op.symbol()), span)
                        .with_help(help),
                );
            }
            _ if left == right && self.is_ordered(left) => {}
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
            (_, Type::Unknown)
            | (UnaryOp::Not, Type::Bool)
            | (UnaryOp::Neg, Type::Int | Type::Float)
            | (UnaryOp::BitNot, Type::Int) => ty,
            (UnaryOp::Neg, Type::Sized(num)) if !num.is_unsigned() => ty,
            (UnaryOp::BitNot, Type::Sized(num)) if !num.is_float() => ty,
            (UnaryOp::Neg, Type::Sized(_)) => {
                let message = format!("can't negate {}, which is never below 0", a(&ty));
                let help = "convert it to a signed size first, like `i64(x)`";
                self.error(Diagnostic::new(message, span).with_help(help));
                Type::Unknown
            }
            (UnaryOp::Not, _) => {
                self.error(
                    Diagnostic::new(format!("`not` needs a bool, but this is {}", article(&ty.kind())), span)
                        .with_help("Fennec has no truthiness: compare explicitly, like `items.len() == 0`"),
                );
                Type::Bool
            }
            (UnaryOp::Neg | UnaryOp::BitNot, Type::Result(..)) => {
                self.error(unhandled_result(inner.span));
                Type::Unknown
            }
            (UnaryOp::Neg | UnaryOp::BitNot, ty) if ty.maybe_none() => {
                self.error(might_be_none(inner.span));
                Type::Unknown
            }
            (UnaryOp::Neg, _) => {
                self.error(Diagnostic::new(format!("can't negate {}", article(&ty.kind())), span));
                Type::Unknown
            }
            (UnaryOp::BitNot, _) => {
                self.error(not_bits(BinOp::BitXor, "~", &ty, span));
                Type::Unknown
            }
        }
    }

    /// `object.name`, which takes a `shared` handle's lock to read it.
    fn read_attr(&mut self, object: &Expr, name: &str, optional: bool, span: Span) -> Type {
        let ty = self.attr(object, name, optional, span);
        if let Some(held) = self.recorded(object.span) {
            self.locks_again(object, &held, &format!("reading `{name}`"));
        }
        ty
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
            // What it's read from still has a type, for compilers to read it through.
            self.expr(object);
            return ty;
        }
        let ty = self.expr(object);
        if let Some(field) = self.field_attr(&ty, name, span) {
            return field;
        }
        if self.outside_lock(object, &ty, span) {
            return Type::Unknown;
        }
        let ty = self.failing_step(ty, object.span, span);
        let optional = optional || self.chained_step(&ty, span);
        self.field_of(&ty, name, optional, span)
    }

    /// A field or method of something that can fail, read in the chain before an `else`, which
    /// stops at the first failing step too. It's read as what's inside it or `None`, as it's written
    /// in: `(value else None).step`.
    pub(crate) fn failing_step(&mut self, ty: Type, object: Span, step: Span) -> Type {
        let Type::Result(inner, _) = &ty else { return ty };
        if !self.else_chain.contains(&(step.start, step.end)) {
            return ty;
        }
        let read_as = Type::optional(inner.as_ref().clone());
        self.checker.failing_steps.insert((step.start, step.end));
        // Expressions are told apart by span, and nothing's written here, so it's empty.
        self.record(Span { start: object.end, end: object.end }, &read_as);
        read_as
    }

    /// Whether a field or method of a value that might be missing is read in the chain before
    /// an `else`, which stops at the first missing step. Those are written in as optional.
    pub(crate) fn chained_step(&mut self, ty: &Type, step: Span) -> bool {
        let chained =
            matches!(ty, Type::Optional(_) | Type::None) && self.else_chain.contains(&(step.start, step.end));
        if chained {
            self.checker.optional_steps.insert((step.start, step.end));
        }
        chained
    }

    pub(crate) fn field_of(&mut self, ty: &Type, name: &str, optional: bool, span: Span) -> Type {
        match ty {
            Type::Unknown => Type::Unknown,
            Type::None if optional => Type::None,
            Type::Optional(inner) if optional => Type::optional(self.field_of(inner, name, false, span)),
            ty if ty.maybe_none() => {
                self.error(crate::messages::missing_step(span));
                Type::Unknown
            }
            Type::Result(..) => {
                self.error(unhandled_result(span));
                Type::Unknown
            }
            Type::Named(type_name) if type_name == "Args" && name == "program" => Type::Str,
            Type::Named(type_name) | Type::Generic(type_name, _) => {
                let info = self.checker.program.type_info(type_name);
                match info.map(|info| (info.kind, info.def.cases.is_empty())) {
                    Some((kind, false)) => {
                        let what = if kind == TypeKind::Enum { "is an enum" } else { "has cases" };
                        self.error(
                            Diagnostic::new(
                                format!("`{type_name}` {what}, so its fields depend on its case"),
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
                            self.private_member(type_name, name, span);
                            let field = self.checker.field_type(type_name, None, name);
                            return field.substitute(&self.checker.program.type_args(ty));
                        }
                        self.error(
                            Diagnostic::new(format!("`{type_name}` has no field `{name}`"), span)
                                .with_help("call a method with parentheses, like `x.length()`"),
                        );
                        Type::Unknown
                    }
                    None => self.no_field(ty, name, span),
                }
            }
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
        if info.is_some_and(|info| info.def.cases.iter().any(|case| case.name == name)) {
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

    /// The type of each item when looping over a value.
    pub(crate) fn item_type(&mut self, ty: &Type, span: Span) -> Type {
        match self.checker.settled(ty) {
            Type::List(item)
            | Type::Array(item, _)
            | Type::Buffer(item)
            | Type::Set(item)
            | Type::Iterator(item)
            | Type::Table(item)
            | Type::Dict(item, _) => *item,
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
            Type::Bytes => Type::Int,
            Type::Unknown | Type::Var(_) => Type::Unknown,
            Type::Result(..) => {
                self.error(unhandled_result(span));
                Type::Unknown
            }
            ty if ty.maybe_none() => {
                self.error(might_be_none(span));
                Type::Unknown
            }
            // `for arg in args` gives the command-line arguments.
            Type::Named(name) if name == "Args" => Type::Str,
            ty @ (Type::Named(_) | Type::Generic(..)) => self.iterated(&ty, span),
            other => {
                self.error(Diagnostic::new(format!("can't loop over {}", article(&other.kind())), span));
                Type::Unknown
            }
        }
    }

    /// The items of a type looped over with its own `def iter`.
    fn iterated(&mut self, ty: &Type, span: Span) -> Type {
        let name = ty.type_name().unwrap_or_default();
        if let Some((key, _)) = self.checker.program.method(name, "iter") {
            return match self.method_signature(ty, &key).map(|signature| signature.returns) {
                Some(Type::List(item) | Type::Iterator(item)) => *item,
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
        kept: bool,
    ) -> (Type, Type) {
        let (elem, value, cond) = parts;
        let iter_ty = self.expr(iter);
        let item = self.item_type(&iter_ty, iter.span);
        let before = self.snapshot();
        // One kept for later runs after this line, so it can't change what's around it.
        self.push(kept);
        self.value_functions += 1;
        self.bind_names(names, &item, false, (iter.span, span));
        if let Some(cond) = cond {
            self.condition(cond, "if");
            self.track(cond);
            self.narrow(&not_none(cond, true));
        }
        let elem = self.expr(elem);
        let value = value.map_or(Type::Unit, |value| self.expr(value));
        self.value_functions -= 1;
        self.pop();
        self.restore(&before);
        (elem, value)
    }

    /// `value else default`: what's inside the value, or the default, or nothing more if the default
    /// leaves.
    fn else_value(&mut self, value: &Expr, default: &Expr, span: Span) -> Type {
        let mut steps = Vec::new();
        else_steps(value, &mut steps);
        self.else_chain.extend(steps.iter().copied());
        let written = value.span;
        let value = self.expr(value);
        for step in &steps {
            self.else_chain.remove(step);
        }
        // On `T? fails E`, `?` handles the failure and `else` the `None`: a plain `else` would
        // handle both, so only `else as why:` takes the failure, giving a `T?`.
        if let Type::Result(inner, _) = self.checker.settled(&value)
            && matches!(self.checker.settled(&inner), Type::Optional(_) | Type::None)
            && default.failure_named().is_none()
        {
            self.error(swallows_failure(written, &inner));
        }
        let mut leaves = default.leaves();
        if leaves && self.value_functions > 0 {
            let message = "an arrow function or comprehension can't leave with `else`";
            self.error(
                Diagnostic::new(message, default.span)
                    .with_help("write a loop, or give a value after `else`"),
            );
        }
        let default = match default.else_block() {
            Some((failure, block)) => {
                let given = self.else_block(failure, block, &value, written);
                leaves |= given.is_none();
                let given = given.unwrap_or(Type::Unit);
                // The block is an expression too, typed as what it gives, over the value nothing
                // wrote that it's written in with.
                self.record(default.span, &given);
                if let ExprKind::Match { subject, .. } = &default.kind {
                    self.record(subject.span, &Type::Bool);
                }
                given
            }
            None => self.expr(default),
        };
        match value {
            Type::Optional(inner) if matches!(*inner, Type::Result(..)) => {
                self.error(
                    Diagnostic::new("two steps of this chain can fail, and `else` stops at one", span)
                        .with_help("get the first step's value out with its own `else` or `?` first"),
                );
                Type::Unknown
            }
            Type::Optional(inner) | Type::Result(inner, _) if leaves => *inner,
            Type::Optional(inner) | Type::Result(inner, _) => {
                self.join_or_report(&[*inner, default], span, "the two sides of this `else` give")
            }
            Type::None => default,
            other => other,
        }
    }

    /// An `else:` block, or `else as failure:`, whose block sees what failed under that name. It
    /// leaves, and then gives `None` here, or its last line is the value in place of the one that
    /// failed or was `None`, of that one's type.
    fn else_block(
        &mut self,
        failure: Option<&str>,
        block: &[fennec_syntax::Stmt],
        value: &Type,
        written: Span,
    ) -> Option<Type> {
        let settled = self.checker.settled(value);
        let error =
            failure.map(|_| match &settled {
                Type::Result(_, error) => (**error).clone(),
                Type::Unknown => Type::Unknown,
                _ => {
                    let message = "this can't fail, so `else as` has no failure to name";
                    self.error(Diagnostic::new(message, written).with_help(
                        "write `else:` for a value that's `None`, or call something that can fail",
                    ));
                    Type::Unknown
                }
            });
        let instead = match &settled {
            Type::Optional(inner) | Type::Result(inner, _) => Some((**inner).clone()),
            _ => None,
        };
        self.push(false);
        if let (Some(failure), Some(error)) = (failure, error) {
            self.declare(failure, error, false, written);
        }
        let tail = std::mem::replace(&mut self.tail, instead.clone());
        let flow = self.stmts(block, true);
        self.tail = tail;
        self.pop();
        if flow.exits {
            return None;
        }
        let gives_nothing = matches!(self.checker.settled(&flow.ty), Type::Unit)
            || !matches!(block.last().map(|last| &last.kind), Some(StmtKind::Expr(_)));
        match instead {
            Some(instead) if gives_nothing && !matches!(self.checker.settled(&instead), Type::Unit) => {
                if let Some(last) = block.last() {
                    self.error(gives_nothing_or_leaves(&instead, last));
                }
                Some(instead)
            }
            _ => Some(flow.ty),
        }
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
        self.value_functions += 1;
        self.arrows += 1;
        let returns = self.expr(body);
        self.arrows -= 1;
        self.value_functions -= 1;
        self.pop();
        let ty = Type::Arrow(known, Box::new(returns));
        self.record(span, &ty);
        ty
    }

    /// `value?`: the value inside a result, passing its error up.
    fn try_value(&mut self, value: &Expr, context: Option<&Expr>, span: Span) -> Type {
        // A function used as a value says nothing of failing, so nothing is told to expect it.
        let in_arrow = self.arrows > 0;
        if in_arrow {
            self.error(
                Diagnostic::new("an arrow function can't pass a failure up with `?`", span)
                    .with_help("give a value with `else` here, or write a loop, which can use `?`"),
            );
        }
        let ty = self.expr(value);
        let inner = match &ty {
            Type::Result(inner, _) if in_arrow => inner.as_ref().clone(),
            Type::Result(inner, error) => {
                if context.is_none() {
                    self.errors.push(error.as_ref().clone());
                }
                inner.as_ref().clone()
            }
            Type::Unknown => Type::Unknown,
            other => {
                let help = if other.maybe_none() {
                    "`?` passes failures up, not None: end the chain with `else` and a default, or check with `if x is None:`"
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
            if !in_arrow {
                self.errors.push(error);
            }
        }
        inner
    }
}

/// A bit operator on something other than an int, with the fix for what was probably meant.
fn not_bits(op: BinOp, symbol: &str, ty: &Type, span: Span) -> Diagnostic {
    let diagnostic =
        Diagnostic::new(format!("`{symbol}` works on ints, but this is {}", article(&ty.kind())), span);
    match (ty, op) {
        (Type::Bool, BinOp::BitAnd) => diagnostic.with_help("for bools, write `and`"),
        (Type::Bool, BinOp::BitOr) => diagnostic.with_help("for bools, write `or`"),
        (Type::Bool, _) => diagnostic.with_help("for bools, write `not` or `!=`"),
        (Type::Float, _) => diagnostic.with_help("convert it with int(...)"),
        (Type::Set(_), _) => {
            diagnostic.with_help("sets don't have bit operators: use a loop or a comprehension")
        }
        _ => diagnostic,
    }
}

/// The fields and method calls along the chain before an `else`: `a.b().c else d` has `a.b`
/// and `a.b().c`, but nothing inside the arguments.
fn else_steps(expr: &Expr, steps: &mut Vec<(u32, u32)>) {
    match &expr.kind {
        ExprKind::Attr { object, .. } => {
            steps.push((expr.span.start, expr.span.end));
            else_steps(object, steps);
        }
        ExprKind::Call { callee, .. } => else_steps(callee, steps),
        ExprKind::Index { object, .. } => else_steps(object, steps),
        _ => {}
    }
}

/// Whether two number types of different sizes meet, one of them `int`, `float` or another size:
/// sizes never mix on their own.
fn sizes_mixed(left: &Type, right: &Type) -> bool {
    let number = |ty: &Type| matches!(ty, Type::Int | Type::Float | Type::Sized(_));
    left != right
        && number(left)
        && number(right)
        && (matches!(left, Type::Sized(_)) || matches!(right, Type::Sized(_)))
}

/// What to write to make one side the other's size.
fn convert_help(to: &Type) -> String {
    format!("convert one side, like `{to}(x)`: numbers of different sizes never mix on their own")
}

/// An operator between numbers of different sizes, like `i32 + i64`.
fn mixed_sizes(op: BinOp, left: &Type, right: &Type, span: Span) -> Diagnostic {
    Diagnostic::new(format!("can't mix {left} and {right} in `{}`", op.symbol()), span)
        .with_help(convert_help(left))
}

/// An `else` block that ends without a value or a way out, said with what it has to give and what
/// its last line is.
fn gives_nothing_or_leaves(instead: &Type, last: &fennec_syntax::Stmt) -> Diagnostic {
    let shown = a(instead);
    let wanted = match shown.split_once(' ') {
        Some((word @ ("a" | "an"), name)) => format!("{word} `{name}`"),
        _ => format!("`{shown}`"),
    };
    let message = format!(
        "this `else` block must give {wanted} or leave; its last line {} gives nothing",
        line_shown(last)
    );
    Diagnostic::new(message, last.span).with_help("add a value, or `return`, `fail`, `break` or `continue`")
}

/// A line as a message shows it, short: `log.warn(...)`, `total = ...`, or what kind of line it is.
fn line_shown(line: &fennec_syntax::Stmt) -> String {
    match &line.kind {
        StmtKind::Expr(Expr { kind: ExprKind::Call { callee, .. }, .. }) => fennec_syntax::dotted(callee)
            .map_or_else(|| "a call".to_owned(), |called| format!("`{called}(...)`")),
        StmtKind::Assign { target, .. } => fennec_syntax::dotted(target)
            .map_or_else(|| "an assignment".to_owned(), |name| format!("`{name} = ...`")),
        StmtKind::If { .. } => "an `if`".to_owned(),
        StmtKind::For { .. } => "a `for` loop".to_owned(),
        StmtKind::While { .. } => "a `while` loop".to_owned(),
        StmtKind::With { .. } => "a `with` block".to_owned(),
        StmtKind::Pass => "`pass`".to_owned(),
        _ => "this".to_owned(),
    }
}

/// A plain `else` on what can fail as well as give `None`, which would handle both.
fn swallows_failure(written: Span, inner: &Type) -> Diagnostic {
    let end = Span { start: written.end, end: written.end };
    Diagnostic::new("this can fail as well as give `None`, and a plain `else` would handle both", written)
        .with_help(format!(
            "pass the failure up with `?` first, so `else` handles the `None`, or handle the failure with `else as why:`, whose block gives {}",
            a(inner)
        ))
        .with_hint("pass the failure up", end, "?")
}
