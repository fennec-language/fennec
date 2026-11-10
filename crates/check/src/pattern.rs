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
        // The errors earlier cases took all of, which a later `case Err(name)` can't hold.
        let mut taken: Vec<String> = Vec::new();
        // One `case Ok` or `case Err` a program wrote is enough to say they're gone.
        let gone = arms.iter().find(|arm| {
            crate::gone::written_by_hand(arm.span)
                && alternatives(&arm.pattern).iter().any(|pattern| {
                    matches!(pattern, Pattern::Variant { name, dotted: false, .. } if name == "Ok" || name == "Err")
                })
        });
        if let Some(arm) = gone {
            self.error(crate::gone::result_case(arm.span));
        }
        for arm in arms {
            self.push(false);
            self.narrow_to_case(subject, &arm.pattern);
            self.note_undotted_case(&arm.pattern, &ty, arm.span);
            let left = self.without_taken(&ty, &taken);
            self.bind_pattern(&arm.pattern, &left, arm.span);
            if arm.guard.is_none() {
                taken.extend(self.error_taken(&arm.pattern, &ty));
            }
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
        let refs = arms.iter().any(|arm| arm.pattern.holds_ref());
        if refs {
            self.ref_cases(subject, arms, &ty, span);
        }
        if values.is_empty() {
            return Type::Unit;
        }
        let value = self.join_or_report(&values, span, "the cases of this `match` give");
        let statement = self.statement_matches.contains(&(span.start, span.end));
        if refs && statement && !matches!(self.checker.settled(&value), Type::Unit | Type::Unknown) {
            self.error(
                Diagnostic::new("a `match` with a `ref` case gives back nothing", span)
                    .with_help("it changes what it's over: give back a value after it, on a line of its own"),
            );
        }
        value
    }

    /// A case naming a member of the union its subject holds, as `case Circle(radius=_):`,
    /// narrows the subject to that member inside the case, as `is` does.
    fn narrow_to_case(&mut self, subject: &Expr, pattern: &Pattern) {
        let Some(path) = crate::stmt::path(subject) else { return };
        if let Pattern::Variant { name, dotted: false, .. } = pattern {
            self.narrow_members(&[(path, name.clone(), true)]);
        }
    }

    /// A `match` with a `ref` case changes what it's over: it's a statement of its own, over a
    /// place held in a mut name, and each `ref` is a field of the case it's in, beside names,
    /// `_`s and literals, so the case can be put back whole where it ends.
    fn ref_cases(&mut self, subject: &Expr, arms: &[MatchArm], ty: &Type, span: Span) {
        if !self.statement_matches.contains(&(span.start, span.end)) {
            self.error(
                Diagnostic::new("a `match` with a `ref` case is a statement of its own", span)
                    .with_help("it changes what it's over and gives back nothing: write it on its own line"),
            );
        }
        if plain_place(subject) {
            self.mut_place(
                subject,
                "a `ref` case changes what the `match` is over, so it has to be held in a mut name",
            );
        } else {
            self.error(
                Diagnostic::new(
                    "a `ref` case changes what the `match` is over, which has to be a place",
                    subject.span,
                )
                .with_help(
                    "match a name, a field or an item at a name's position, like `match ui[widget_id].kind:`",
                ),
            );
        }
        let owner = self.checker.settled(ty).type_name().map(str::to_owned);
        let mut cases = Vec::new();
        for arm in arms {
            let Pattern::Variant { name, args, .. } = &arm.pattern else {
                if arm.pattern.holds_ref() {
                    self.error(
                        Diagnostic::new("`ref` names a field of the case it's in", arm.span).with_help(
                            "write it inside a case's parentheses, like `case .Button(ref clicks):`",
                        ),
                    );
                }
                cases.push(None);
                continue;
            };
            if !args.iter().any(|arg| matches!(arg, Pattern::RefBind(_))) {
                if arm.pattern.holds_ref() {
                    self.error(
                        Diagnostic::new("`ref` names a field of the case it's in", arm.span).with_help(
                            "write it right inside the case's parentheses, like `case .Button(ref clicks):`",
                        ),
                    );
                }
                cases.push(None);
                continue;
            }
            if args.iter().any(|arg| {
                !matches!(
                    arg,
                    Pattern::RefBind(_) | Pattern::Bind(_) | Pattern::Wildcard | Pattern::Literal(_)
                )
            }) {
                self.error(
                    Diagnostic::new(
                        "a case with a `ref` matches its other fields by name, `_` or a literal",
                        arm.span,
                    )
                    .with_help("match what's inside the field after, in the case's body"),
                );
            }
            cases.push(owner.as_ref().map(|owner| self.field_names(owner, Some(name))));
        }
        self.checker.ref_matches.insert((span.start, span.end), cases);
    }

    /// A result's type with the errors `taken` by earlier cases left out of its error, when that
    /// leaves one: `case Err(error)` after `case Err(Timeout(...))` on `fails SqlError | Timeout`
    /// holds a `SqlError`.
    fn without_taken(&self, subject: &Type, taken: &[String]) -> Type {
        let settled = self.checker.settled(subject);
        let left = |members: &[String]| -> Option<Type> {
            let left: Vec<String> =
                members.iter().filter(|member| !taken.contains(member)).cloned().collect();
            let [one] = left.as_slice() else { return None };
            Some(Type::Named(one.clone()))
        };
        match &settled {
            Type::Result(value, error) => match error.as_ref() {
                Type::Errors(members) => left(members)
                    .map_or_else(|| subject.clone(), |error| Type::Result(value.clone(), Box::new(error))),
                _ => subject.clone(),
            },
            // What `else as why:` names when a call can fail several ways: `match why:`, whose
            // later cases hold what the earlier ones left.
            Type::Errors(members) => left(members).unwrap_or_else(|| subject.clone()),
            _ => subject.clone(),
        }
    }

    /// The error a `case Err(X(...))` takes all of: a plain error `X` of the result's union, with
    /// nothing but names and `_` inside.
    fn error_taken(&self, pattern: &Pattern, subject: &Type) -> Option<String> {
        let plain =
            |taken: &str| self.checker.program.type_info(taken).is_some_and(|info| info.def.cases.is_empty());
        match self.checker.settled(subject) {
            Type::Result(_, error) => {
                let Type::Errors(members) = *error else { return None };
                let Pattern::Variant { name, dotted: false, args, .. } = pattern else { return None };
                let [Pattern::Variant { name: taken, dotted: false, args: inner, .. }] = args.as_slice()
                else {
                    return None;
                };
                (name == "Err" && plain(taken) && members.contains(taken) && inner.iter().all(always_matches))
                    .then(|| taken.clone())
            }
            // `case Gone(path):` on what failed one of several ways takes all of `Gone`.
            Type::Errors(members) => {
                let Pattern::Variant { name: taken, dotted: false, args: inner, .. } = pattern else {
                    return None;
                };
                (plain(taken) && members.contains(taken) && inner.iter().all(always_matches))
                    .then(|| taken.clone())
            }
            _ => None,
        }
    }

    /// Notes a `case` that names a case of the enum or error being matched without its dot. On a
    /// result, a name with no dot means a case of the error and a dot means one of the value, so
    /// a dot wouldn't mean the same there, and nothing's noted.
    fn note_undotted_case(&mut self, pattern: &Pattern, subject: &Type, span: Span) {
        let Pattern::Variant { name, dotted: false, .. } = pattern else { return };
        let Type::Named(owner) = self.checker.settled(subject) else { return };
        // `case Kind.One:` where the type is known is written `case .One:`.
        if let Some((written, case)) = name.rsplit_once('.') {
            let shown = fennec_syntax::rename::shown_type_name(&owner);
            if written != shown && written != owner {
                return;
            }
            if span.in_prelude() {
                return;
            }
            let start = span.start + 5;
            let prefix = u32::try_from(written.len()).unwrap_or(0) + 1;
            let message = format!("the type here is known, so write this case as `.{case}`");
            self.error(
                Diagnostic::new(message, span)
                    .with_help("`fennec fix` makes this change")
                    .with_fix(Span { start, end: start + prefix }, "."),
            );
            return;
        }
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

    /// `assert load(text) fails Oops(line=3)`: what's asserted can fail, and the pattern is a case
    /// of what it fails with, its names seen only in it.
    pub(crate) fn assert_fails(&mut self, value: &Expr, pattern: &Pattern, span: Span) {
        let ty = self.expr(value);
        let error = match self.checker.settled(&ty) {
            Type::Result(_, error) => *error,
            Type::Unknown => Type::Unknown,
            other => {
                let message = "this can't fail, so `assert ... fails` has nothing to check";
                self.error(
                    Diagnostic::new(message, value.span)
                        .with_help(format!("it gives {}: compare it with `==` instead", a(&other))),
                );
                Type::Unknown
            }
        };
        self.push(false);
        self.bind_pattern(pattern, &error, span);
        self.pop();
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
            // A name for the field itself, which the case's body may change: the `match` is over
            // a place it changes, checked where the `match` is.
            Pattern::RefBind(name) => self.declare(name, value, true, span),
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
            Pattern::Variant { name, dotted, .. } => {
                let failure =
                    matches!(subject, Type::Result(..)) && !dotted && self.failure_side(name, &error);
                self.bind_variant(pattern, if failure { &error } else { &value }, span);
            }
        }
    }

    /// `.Circle(r)`, `IoError(reason=why)` or `Point(x, _)`: each part bound to its field, found by
    /// position or by the name it's given.
    fn bind_variant(&mut self, pattern: &Pattern, target: &Type, span: Span) {
        let Pattern::Variant { name, dotted, args, fields: given } = pattern else { return };
        let dotted = *dotted;
        let owner = self.variant_owner(name, dotted, target, span);
        let fields = owner.as_ref().map(|(owner, case)| self.field_names(owner, case.as_deref()));
        let types = owner.as_ref().map(|(owner, case)| self.case_fields(owner, case.as_deref()));
        if let Some((owner, _)) = &owner
            && given.is_empty()
            && !args.is_empty()
        {
            self.positional_elsewhere(owner, name, fields.as_deref().unwrap_or_default(), span);
        }
        if let Some(fields) = &fields
            && given.is_empty()
            && args.len() > fields.len()
        {
            let message =
                format!("this pattern has {} parts, but the value has {} fields", args.len(), fields.len());
            self.error(Diagnostic::new(message, span));
        }
        // Where each part goes among the fields: by position, or by the name it's given.
        let places: Vec<Option<usize>> = if given.is_empty() {
            (0..args.len()).map(Some).collect()
        } else {
            given.iter().map(|field| self.named_place(name, field, fields.as_deref(), span)).collect()
        };
        if !given.is_empty()
            && let Some(fields) = &fields
        {
            self.checker.named_patterns.insert((span.start, span.end, name.to_owned()), fields.clone());
        }
        let types = types.unwrap_or_default();
        // A struct's or error's `_` fields can't be read from outside it, here either.
        let private = self.checker.program.type_info(name).is_some_and(|_| !dotted);
        for (arg, place) in args.iter().zip(places) {
            if private
                && let Some(field) = place.and_then(|at| fields.as_ref().and_then(|fields| fields.get(at)))
                && !matches!(arg, Pattern::Wildcard)
            {
                self.private_member(name, &field.clone(), span);
            }
            let field = place.and_then(|at| types.get(at)).cloned().unwrap_or(Type::Unknown);
            self.bind_pattern(arg, &field, span);
        }
    }

    /// Where the field a pattern names is among its type's fields, reporting a name it doesn't have.
    fn named_place(
        &mut self,
        variant: &str,
        field: &str,
        names: Option<&[String]>,
        span: Span,
    ) -> Option<usize> {
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
        // `case Err(SqlError.Failed(reason))`: the type and its case, written out.
        if let Some((written, case)) = name.rsplit_once('.')
            && self
                .checker
                .program
                .type_info(written)
                .is_some_and(|info| info.def.cases.iter().any(|known| known.name == case))
        {
            return Some((written.to_owned(), Some(case.to_owned())));
        }
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
            [owner] if dotted && *subject == Type::Unknown => Some((owner.clone(), Some(name.to_owned()))),
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
        let subject = &self.checker.settled(subject);
        if let Type::Result(value, error) = subject {
            return self.check_result_cases(value, error, arms, span);
        }
        if let Type::Union(members) = subject {
            return self.check_members(members, arms, span);
        }
        // What `else as why:` names when a call can fail several ways: each has a case, or `_`.
        if let Type::Errors(members) = subject {
            let unguarded: Vec<&Pattern> = arms
                .iter()
                .filter(|arm| arm.guard.is_none())
                .flat_map(|arm| alternatives(&arm.pattern))
                .collect();
            let missing: Vec<String> = members
                .iter()
                .filter(|member| !self.covers(&Type::named(member.as_str()), &unguarded))
                .map(|member| format!("`{}`", fennec_syntax::rename::shown_type_name(member)))
                .collect();
            if !missing.is_empty() {
                let message = format!("this `match` doesn't handle {}", missing.join(" or "));
                self.error(Diagnostic::new(message, span).with_help(
                    "add a case for each error it can fail with, or `case _:` for everything else",
                ));
            }
            return;
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

impl FnCx<'_> {
    /// A `match` over what a call that can fail gives back has to handle its value and its
    /// failure, or have `case _`: one with only `case Ok(data):` would stop the program at a
    /// failure.
    fn check_result_cases(&mut self, value: &Type, error: &Type, arms: &[MatchArm], span: Span) {
        let unguarded: Vec<&Pattern> = arms
            .iter()
            .filter(|arm| arm.guard.is_none())
            .flat_map(|arm| alternatives(&arm.pattern))
            .collect();
        // Only `_` matches either side: a name, like `case found:`, is bound to the value.
        if unguarded.iter().any(|pattern| matches!(pattern, Pattern::Wildcard)) {
            return;
        }
        // `Ok(x)` and `.Case(...)` are the value's cases, and `Err(e)`, `IoError(...)` and a case
        // of what it fails with, `NotFound(path)`, the failure's, as `bind_pattern` reads them.
        let mut values = Vec::new();
        let mut failures = Vec::new();
        for pattern in unguarded {
            match pattern {
                Pattern::Variant { name, dotted: false, args, .. } if name == "Ok" || name == "Err" => {
                    let side = if name == "Ok" { &mut values } else { &mut failures };
                    match args.as_slice() {
                        [] => side.push(&Pattern::Wildcard),
                        [one] => side.extend(alternatives(one)),
                        _ => {}
                    }
                }
                Pattern::Variant { name, dotted: false, .. } if self.failure_side(name, error) => {
                    failures.push(pattern);
                }
                _ => values.push(pattern),
            }
        }
        if !self.covers(value, &values) {
            self.error(
                Diagnostic::new("this `match` doesn't handle the value it gives when it works", span)
                    .with_help("add `case Ok(value):`, or `case _:` for everything else"),
            );
        }
        if !self.covers(error, &failures) {
            self.error(
                Diagnostic::new("this `match` doesn't handle the failure", span).with_help(
                    "add `case Err(error):`, a case for each error it can fail with, or `case _:` for everything else",
                ),
            );
        }
    }

    /// Whether an undotted name in a `case` over a result is the failure's: an error type, or a case
    /// of one it fails with. Lowering reads it the same way.
    fn failure_side(&self, name: &str, error: &Type) -> bool {
        let failing = match error {
            Type::Named(one) => vec![one.as_str()],
            Type::Errors(names) => names.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        };
        self.checker.is_error_type(&Type::named(name))
            || failing.into_iter().any(|owner| {
                self.checker
                    .program
                    .type_info(owner)
                    .is_some_and(|info| info.def.cases.iter().any(|case| case.name == name))
            })
    }

    /// Whether `patterns` match every value of type `ty`: one that matches anything, every case
    /// of an enum, every type of a union, or the one type of a struct or error.
    fn covers(&self, ty: &Type, patterns: &[&Pattern]) -> bool {
        if patterns.iter().any(|pattern| always_matches(pattern)) {
            return true;
        }
        let whole: Vec<&str> = patterns
            .iter()
            .filter_map(|pattern| match pattern {
                Pattern::Variant { name, args, .. } if args.iter().all(always_matches) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        match ty {
            Type::Unknown => true,
            Type::Errors(names) => names.iter().all(|name| {
                whole.contains(&name.as_str()) || self.covers(&Type::Named(name.clone()), patterns)
            }),
            Type::Union(members) => members
                .iter()
                .all(|member| whole.contains(&member.to_string().as_str()) || self.covers(member, patterns)),
            Type::Named(owner) => match self.checker.program.type_info(owner) {
                // A case is named on its own, `.Stuck`, or with its type, `Shut.Stuck`.
                Some(info) if !info.def.cases.is_empty() => info.def.cases.iter().all(|case| {
                    whole.iter().any(|written| {
                        let (owner_written, case_written) = written.rsplit_once('.').unwrap_or(("", written));
                        case_written == case.name
                            && (owner_written.is_empty()
                                || owner_written == owner.as_str()
                                || owner_written == fennec_syntax::rename::shown_type_name(owner))
                    })
                }),
                _ => whole.contains(&owner.as_str()),
            },
            Type::Bool => {
                let literal = |value: bool| {
                    patterns.iter().any(|pattern| {
                        matches!(pattern, Pattern::Literal(expr) if expr.kind == ExprKind::Bool(value))
                    })
                };
                literal(true) && literal(false)
            }
            _ => false,
        }
    }
}

/// Whether a pattern matches anything, so a case using it covers every value of that case.
fn always_matches(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) => true,
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

/// Whether an expression is a place that reads the same each time: a name, a field of one, or an
/// item at a position written as a name or a number, as the case is put back there where it ends.
fn plain_place(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(_) => true,
        ExprKind::Attr { object, optional: false, .. } => plain_place(object),
        ExprKind::Index { object, index } => {
            plain_place(object) && matches!(index.kind, ExprKind::Name(_) | ExprKind::Int(_))
        }
        _ => false,
    }
}
