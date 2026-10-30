//! `match`: the names each `case` binds, its guard, and whether the cases cover every case
//! of an enum. A guarded case never counts toward covering one.
//! Matching a result looks through `Ok`: `case Ok(x)` and `case x` bind the value, and
//! `case Err(e)` or `case NotFound(path)` the error.

use fennec_syntax::{Diagnostic, Expr, ExprKind, MatchArm, Pattern, Span};

use crate::function::FnCx;
use crate::program::TypeKind;
use crate::types::{Type, a};

impl FnCx<'_> {
    /// A `match`. `wanted` is the type expected of its value, if any, which each case's
    /// value is expected to have too.
    pub(crate) fn match_expr(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        span: Span,
        wanted: Option<Type>,
    ) -> Type {
        let ty = self.expr(subject);
        let outer = std::mem::replace(&mut self.tail, wanted);
        let before = self.snapshot();
        let mut paths = Vec::new();
        let mut values = Vec::new();
        for arm in arms {
            self.push(false);
            self.bind_pattern(&arm.pattern, &ty, arm.span);
            if let Some(guard) = &arm.guard {
                self.guard(guard);
            }
            let flow = self.stmts(&arm.body, true);
            self.pop();
            if !flow.exits {
                paths.push(self.snapshot());
                values.push(flow.ty);
            }
            self.restore(&before);
        }
        self.tail = outer;
        self.merge(&paths);
        self.check_cases(&ty, arms, subject.span);
        if values.is_empty() {
            return Type::Unit;
        }
        self.join_or_report(&values, span, "the cases of this `match` give")
    }

    fn bind_pattern(&mut self, pattern: &Pattern, subject: &Type, span: Span) {
        let subject = &self.checker.settled(subject);
        let (value, error) = match subject {
            Type::Result(value, error) => (value.as_ref().clone(), error.as_ref().clone()),
            other => (other.clone(), Type::Unknown),
        };
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Literal(literal) => {
                let ty = self.expr(literal);
                if !self.checker.fits(&value, &ty) && !self.checker.fits(&ty, &value) {
                    let message =
                        format!("this case can't match: the value is {}, and this is {}", a(&value), a(&ty));
                    self.error(Diagnostic::new(message, literal.span));
                }
            }
            Pattern::Bind(name) => self.declare(name, value, false, span),
            Pattern::Or(alternatives) => self.bind_alternatives(alternatives, subject, span),
            Pattern::Tuple(patterns) => {
                let items = match value {
                    Type::Tuple(items) if items.len() == patterns.len() => items,
                    Type::Unknown => vec![Type::Unknown; patterns.len()],
                    other => {
                        let message = match other {
                            Type::Tuple(items) => format!(
                                "this pattern has {} parts, but the value has {}, so it can't match",
                                patterns.len(),
                                items.len()
                            ),
                            other => format!("this pattern is a tuple, but the value is {}", a(&other)),
                        };
                        self.error(Diagnostic::new(message, span));
                        vec![Type::Unknown; patterns.len()]
                    }
                };
                for (pattern, item) in patterns.iter().zip(&items) {
                    self.bind_pattern(pattern, item, span);
                }
            }
            Pattern::Variant { name, dotted: false, args } if name == "Ok" || name == "Err" => {
                if !matches!(subject, Type::Result(..) | Type::Unknown) {
                    let message = format!("this is {}, not a result, so it has no `{name}` case", a(subject));
                    self.error(Diagnostic::new(message, span));
                }
                let inner = if name == "Ok" { value } else { error };
                for arg in args {
                    self.bind_pattern(arg, &inner, span);
                }
            }
            Pattern::Variant { name, dotted, args } => {
                let target = if matches!(subject, Type::Result(..)) && !dotted { &error } else { &value };
                let fields = self.variant_fields(name, *dotted, target, span);
                if let Some(fields) = &fields
                    && args.len() > fields.len()
                {
                    let message = format!(
                        "this pattern has {} parts, but the value has {} fields",
                        args.len(),
                        fields.len()
                    );
                    self.error(Diagnostic::new(message, span));
                }
                let fields = fields.unwrap_or_default();
                for (at, arg) in args.iter().enumerate() {
                    let field = fields.get(at).cloned().unwrap_or(Type::Unknown);
                    self.bind_pattern(arg, &field, span);
                }
            }
        }
    }

    /// `a | b`: each alternative has to bind the same names, with the same types.
    fn bind_alternatives(&mut self, alternatives: &[Pattern], subject: &Type, span: Span) {
        let mut bound: Vec<Vec<(String, Type)>> = Vec::new();
        for alternative in alternatives {
            self.push(false);
            self.bind_pattern(alternative, subject, span);
            let mut names = self.bound_here();
            names.sort_by(|a, b| a.0.cmp(&b.0));
            bound.push(names);
            self.pop();
        }
        let Some((first, rest)) = bound.split_first() else { return };
        for other in rest {
            let only_some = first
                .iter()
                .map(|(name, _)| name)
                .filter(|name| !other.iter().any(|(bound, _)| bound == *name))
                .chain(
                    other
                        .iter()
                        .map(|(name, _)| name)
                        .filter(|name| !first.iter().any(|(bound, _)| bound == *name)),
                )
                .next();
            if let Some(name) = only_some {
                let message = format!("`{name}` is bound by only some of the alternatives of this `case`");
                self.error(
                    Diagnostic::new(message, span).with_help("every alternative has to bind the same names"),
                );
                return;
            }
            for ((name, ty), (_, other_ty)) in first.iter().zip(other) {
                if !self.checker.fits(ty, other_ty) || !self.checker.fits(other_ty, ty) {
                    let message =
                        format!("`{name}` is {} in one alternative and {} in another", a(ty), a(other_ty));
                    self.error(Diagnostic::new(message, span));
                    return;
                }
            }
        }
        for (name, ty) in first.clone() {
            self.declare(&name, ty, false, span);
        }
    }

    /// A `case`'s guard: a bool that can't change anything, so it never passes `mut`. What
    /// it shows isn't None stays shown in the case's body.
    fn guard(&mut self, guard: &Expr) {
        self.condition(guard, "if");
        if let Some(span) = mut_arg(guard) {
            self.error(
                Diagnostic::new("a guard can't change anything, so it can't pass `mut`", span)
                    .with_help("change it in the case's body instead"),
            );
        }
        self.track(guard);
        self.narrow(&crate::stmt::not_none(guard, true));
    }

    /// The field types of what a `case` names: a case of the enum being matched, an error or
    /// struct type, or a case of whichever enum has it.
    fn variant_fields(&mut self, name: &str, dotted: bool, subject: &Type, span: Span) -> Option<Vec<Type>> {
        if let Type::Named(owner) = subject
            && self.checker.program.kind_of(subject) == Some(TypeKind::Enum)
        {
            let has_case = self
                .checker
                .program
                .type_info(owner)
                .is_some_and(|info| info.def.cases.iter().any(|case| case.name == name));
            if has_case {
                return Some(self.case_fields(owner, Some(name)));
            }
            if dotted {
                self.error(Diagnostic::new(format!("`{owner}` has no case called `{name}`"), span));
                return None;
            }
        }
        let kind = self.checker.program.type_info(name).map(|info| info.kind);
        if !dotted && matches!(kind, Some(TypeKind::Struct | TypeKind::Error)) {
            return Some(self.case_fields(name, None));
        }
        match self.checker.program.enums_with_case(name).as_slice() {
            [owner] if dotted && *subject == Type::Unknown => {
                Some(self.case_fields(&owner.clone(), Some(name)))
            }
            _ => None,
        }
    }

    fn case_fields(&mut self, owner: &str, case: Option<&str>) -> Vec<Type> {
        let names: Vec<String> = self
            .checker
            .program
            .type_info(owner)
            .map(|info| match case {
                Some(case) => info
                    .def
                    .cases
                    .iter()
                    .find(|c| c.name == case)
                    .map(|c| c.fields.iter().map(|field| field.name.clone()).collect())
                    .unwrap_or_default(),
                None => info.def.fields.iter().map(|field| field.name.clone()).collect(),
            })
            .unwrap_or_default();
        names.iter().map(|field| self.checker.field_type(owner, case, field)).collect()
    }

    /// A `match` over an enum has to handle every case, or have `case _`.
    fn check_cases(&mut self, subject: &Type, arms: &[MatchArm], span: Span) {
        let Type::Named(owner) = subject else { return };
        let Some(info) = self.checker.program.type_info(owner).filter(|info| info.kind == TypeKind::Enum)
        else {
            return;
        };
        // A guarded case might not be taken, so it covers nothing.
        let unguarded: Vec<&Pattern> = arms
            .iter()
            .filter(|arm| arm.guard.is_none())
            .flat_map(|arm| alternatives(&arm.pattern))
            .collect();
        if unguarded.iter().any(|pattern| matches!(pattern, Pattern::Wildcard | Pattern::Bind(_))) {
            return;
        }
        let covered: Vec<&str> = unguarded
            .iter()
            .filter_map(|pattern| match pattern {
                Pattern::Variant { name, args, .. } if args.iter().all(always_matches) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<&str> = info
            .def
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .filter(|case| !covered.contains(case))
            .collect();
        if missing.is_empty() {
            return;
        }
        let named: Vec<String> = missing.iter().map(|case| format!("`{owner}.{case}`")).collect();
        let cases: Vec<String> = missing.iter().map(|case| format!("`case .{case}:`")).collect();
        self.error(
            Diagnostic::new(format!("this `match` doesn't handle {}", named.join(" or ")), span)
                .with_help(format!("add {}, or `case _:` for everything else", cases.join(" and "))),
        );
    }
}

/// Whether a pattern matches anything, so a case using it covers every value of that case.
fn always_matches(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Wildcard | Pattern::Bind(_) => true,
        Pattern::Tuple(patterns) => patterns.iter().all(always_matches),
        Pattern::Or(patterns) => patterns.iter().any(always_matches),
        Pattern::Literal(_) | Pattern::Variant { .. } => false,
    }
}

/// The patterns a `case` tries: each alternative of `a | b`, or the one pattern.
fn alternatives(pattern: &Pattern) -> Vec<&Pattern> {
    match pattern {
        Pattern::Or(patterns) => patterns.iter().flat_map(alternatives).collect(),
        other => vec![other],
    }
}

/// Where an expression passes something with `mut`, if it does.
fn mut_arg(expr: &Expr) -> Option<Span> {
    if let ExprKind::Call { args, .. } = &expr.kind
        && let Some(arg) = args.iter().find(|arg| arg.is_mut)
    {
        return Some(arg.value.span);
    }
    crate::effects::children(expr).into_iter().find_map(mut_arg)
}
