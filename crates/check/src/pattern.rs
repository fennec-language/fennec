//! `match`: the names each `case` binds, its guard, and whether the cases cover every case
//! of an enum. A guarded case never counts toward covering one.
//! Matching a result looks through `Ok`: `case Ok(x)` and `case x` bind the value, and
//! `case Err(e)` or `case NotFound(path)` the error.

use fennec_syntax::{Diagnostic, Expr, ExprKind, MatchArm, Pattern, Span};

use crate::function::FnCx;
use crate::named::POSITIONAL;
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
            self.note_undotted_case(&arm.pattern, &ty, arm.span);
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

    /// Notes a `case` that names a case of the enum or error being matched without its dot. On a
    /// result, a name with no dot means a case of the error and a dot means one of the value, so
    /// a dot wouldn't mean the same there, and nothing's noted.
    fn note_undotted_case(&mut self, pattern: &Pattern, subject: &Type, span: Span) {
        let Pattern::Variant { name, dotted: false, .. } = pattern else { return };
        let Type::Named(owner) = self.checker.settled(subject) else { return };
        let info = self.checker.program.type_info(&owner);
        if info.is_some_and(|info| info.def.cases.iter().any(|case| case.name == *name)) {
            if span.in_prelude() {
                return;
            }
            // The pattern starts after `case` and its one space, as `fennec fmt` writes it.
            let start = span.start + 5;
            let message = format!("the type here is known, so write this case as `.{name}`");
            self.error(
                Diagnostic::new(message, span)
                    .with_help("`fennec fix` makes this change")
                    .with_fix(Span { start, end: start }, "."),
            );
        }
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
            Pattern::Variant { name, dotted: false, args, .. } if name == "Ok" || name == "Err" => {
                if !matches!(subject, Type::Result(..) | Type::Unknown) {
                    let message = format!("this is {}, not a result, so it has no `{name}` case", a(subject));
                    self.error(Diagnostic::new(message, span));
                }
                let inner = if name == "Ok" { value } else { error };
                for arg in args {
                    self.bind_pattern(arg, &inner, span);
                }
            }
            Pattern::Variant { name, dotted, args, fields: named } => {
                let target = if matches!(subject, Type::Result(..)) && !dotted { &error } else { &value };
                let owner = self.variant_owner(name, *dotted, target, span);
                let names = owner.as_ref().map(|(owner, case)| self.field_names(owner, case.as_deref()));
                let types = owner.as_ref().map(|(owner, case)| self.case_fields(owner, case.as_deref()));
                if let Some((owner, _)) = &owner
                    && named.is_empty()
                    && !args.is_empty()
                {
                    self.positional_elsewhere(owner, name, names.as_deref().unwrap_or_default(), span);
                }
                if let Some(names) = &names
                    && named.is_empty()
                    && args.len() > names.len()
                {
                    let message =
                        format!("this pattern has {} parts, but the value has {} fields", args.len(), names.len());
                    self.error(Diagnostic::new(message, span));
                }
                // Where each part goes among the fields: by position, or by the name it's given.
                let places: Vec<Option<usize>> = if named.is_empty() {
                    (0..args.len()).map(Some).collect()
                } else {
                    named.iter().map(|field| self.named_place(name, field, names.as_deref(), span)).collect()
                };
                if !named.is_empty()
                    && let Some(names) = &names
                {
                    self.checker.named_patterns.insert((span.start, span.end, name.clone()), names.clone());
                }
                let types = types.unwrap_or_default();
                // A struct's or error's `_` fields can't be read from outside it, here either.
                let private = self.checker.program.type_info(name).is_some_and(|_| !dotted);
                for (arg, place) in args.iter().zip(places) {
                    if private
                        && let Some(field) = place.and_then(|at| names.as_ref().and_then(|names| names.get(at)))
                        && !matches!(arg, Pattern::Wildcard)
                    {
                        self.private_member(name, &field.clone(), span);
                    }
                    let field = place.and_then(|at| types.get(at)).cloned().unwrap_or(Type::Unknown);
                    self.bind_pattern(arg, &field, span);
                }
            }
        }
    }

    /// Where the field a pattern names is among its type's fields, reporting a name it doesn't have.
    fn named_place(&mut self, variant: &str, field: &str, names: Option<&[String]>, span: Span) -> Option<usize> {
        let names = names?;
        let place = names.iter().position(|name| name == field);
        if place.is_none() {
            let has = if names.is_empty() {
                "has no fields".to_owned()
            } else {
                let listed: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
                format!("has {}", listed.join(", "))
            };
            self.error(Diagnostic::new(format!("`{variant}` has no field `{field}`: it {has}"), span));
        }
        place
    }

    /// A pattern matching another kit's type by position. A kit can add a field with a default
    /// to a type it exports without breaking anyone who builds or matches it, so outside the
    /// kit a pattern names the fields it reads, and `fennec fix` adds the names.
    fn positional_elsewhere(&mut self, owner: &str, variant: &str, names: &[String], span: Span) {
        let elsewhere = self.checker.program.type_info(owner).is_some_and(|info| info.prelude);
        if !elsewhere || span.in_prelude() {
            return;
        }
        let message = format!("`{variant}` {POSITIONAL}");
        let help = crate::named::help(variant, names);
        self.error(Diagnostic::new(message, span).with_help(help));
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
                Diagnostic::new("a guard can't change anything, so it can't pass `ref`", span)
                    .with_help("change it in the case's body instead"),
            );
        }
        self.track(guard);
        self.narrow(&crate::stmt::not_none(guard, true));
    }

    /// The field types of what a `case` names: a case of the enum being matched, an error or
    /// struct type, or a case of whichever enum has it.
    /// The type, and its case if it has cases, that a pattern like `.Circle(...)` or `IoError(...)`
    /// matches.
    fn variant_owner(
        &mut self,
        name: &str,
        dotted: bool,
        subject: &Type,
        span: Span,
    ) -> Option<(String, Option<String>)> {
        // `case Err(.Bad(text))` on a result that fails with `ParseError | RangeError`.
        if let Type::Errors(owners) = subject {
            let having: Vec<String> = owners
                .iter()
                .filter(|owner| {
                    self.checker
                        .program
                        .type_info(owner)
                        .is_some_and(|info| info.def.cases.iter().any(|case| case.name == name))
                })
                .cloned()
                .collect();
            if let [owner] = having.as_slice() {
                return Some((owner.clone(), Some(name.to_owned())));
            }
            if dotted {
                let message = format!("none of `{}` has a case called `{name}`", owners.join(" | "));
                self.error(Diagnostic::new(message, span));
                return None;
            }
        }
        if let Type::Named(owner) = subject
            && self.checker.program.type_info(owner).is_some_and(|info| !info.def.cases.is_empty())
        {
            let has_case = self
                .checker
                .program
                .type_info(owner)
                .is_some_and(|info| info.def.cases.iter().any(|case| case.name == name));
            if has_case {
                return Some((owner.clone(), Some(name.to_owned())));
            }
            if dotted {
                self.error(Diagnostic::new(format!("`{owner}` has no case called `{name}`"), span));
                return None;
            }
        }
        let kind = self.checker.program.type_info(name).map(|info| info.kind);
        if !dotted && matches!(kind, Some(TypeKind::Struct | TypeKind::Error)) {
            return Some((name.to_owned(), None));
        }
        match self.checker.program.enums_with_case(name).as_slice() {
            [owner] if dotted && *subject == Type::Unknown => {
                Some((owner.clone(), Some(name.to_owned())))
            }
            _ => None,
        }
    }

    /// The names of a type's fields, or of one of its cases', in order.
    fn field_names(&self, owner: &str, case: Option<&str>) -> Vec<String> {
        self.checker
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
            .unwrap_or_default()
    }

    fn case_fields(&mut self, owner: &str, case: Option<&str>) -> Vec<Type> {
        let names = self.field_names(owner, case);
        names.iter().map(|field| self.checker.field_type(owner, case, field)).collect()
    }

    /// A `match` over a union has to handle each type in it, as `case Circle(...)` does, or have
    /// `case _`.
    fn check_members(&mut self, members: &[Type], arms: &[MatchArm], span: Span) {
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
                Pattern::Variant { name, args, dotted: false, .. } if args.iter().all(always_matches) => {
                    Some(name.as_str())
                }
                _ => None,
            })
            .collect();
        let missing: Vec<String> = members
            .iter()
            .map(ToString::to_string)
            .filter(|member| !covered.contains(&member.as_str()))
            .collect();
        if missing.is_empty() {
            return;
        }
        let named: Vec<String> = missing.iter().map(|member| format!("`{member}`")).collect();
        self.error(
            Diagnostic::new(format!("this `match` doesn't handle {}", named.join(" or ")), span)
                .with_help("add a case for each type in the union, or `case _:` for everything else"),
        );
    }

    /// A `match` over an enum has to handle every case, or have `case _`.
    fn check_cases(&mut self, subject: &Type, arms: &[MatchArm], span: Span) {
        if let Type::Union(members) = subject {
            return self.check_members(members, arms, span);
        }
        let Type::Named(owner) = subject else { return };
        let Some(info) = self.checker.program.type_info(owner).filter(|info| !info.def.cases.is_empty())
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
