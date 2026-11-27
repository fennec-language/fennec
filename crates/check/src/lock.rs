//! `shared T` and `lock`: a value many tasks can reach, made from a plain value with
//! `shared scores = Scores()` and reached only inside `lock scores as current:`, to read it, or
//! `lock scores as ref current:`, to change it. Inside the block the name is what it holds.
//! Once checked, `lowering` writes these in as today's `shared` declarations and `with` blocks,
//! which every engine runs.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span, Stmt, WithName};

use crate::function::{FnCx, Origin};
use crate::stmt::Flow;
use crate::types::{Type, a};

/// The name `shared T` has as a type: a generic of one argument, what it holds.
pub(crate) const SHARED: &str = "shared";

/// What a `shared` type holds, if it's one.
pub(crate) fn held(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Generic(name, args) if name == SHARED => args.first(),
        _ => None,
    }
}

/// Whether a `with` is a `lock` lowering wrote in, by the names it gives what it holds.
pub(crate) fn lowered(names: &[WithName]) -> bool {
    !names.is_empty() && names.iter().all(|held| held.name.ends_with(crate::lock_lowering::HELD))
}

/// `shared T` for what a plain value of type `held` is.
pub(crate) fn shared(held: Type) -> Type {
    Type::Generic(SHARED.to_owned(), vec![held])
}

/// Why a `shared` value can't hold a type with a type parameter in it, as `shared list[T]` in a
/// generic struct or function: each `shared` type is made into a declaration of its own when the
/// program's lowered, one for each type it's written with, and a generic's `T` stands for several.
pub(crate) fn shared_of_a_type_parameter(span: Span) -> Diagnostic {
    Diagnostic::new("a `shared` value of a type parameter, as `shared list[T]`, isn't built yet", span).with_help(
        "write out the type it holds, as `shared list[int]`, outside generic code, and pass the shared value to what needs it",
    )
}

impl FnCx<'_> {
    /// `shared scores = Scores()`: `scores` is a new `shared` value holding what's given.
    pub(crate) fn shared_declaration(&mut self, name: &str, value: &Expr, at: Span) {
        let ty = self.expr(value);
        if self.checker.program.is_shared(&ty) {
            let message = format!("this is {} already, so `shared` here has nothing to do", a(&ty));
            self.error(
                Diagnostic::new(message, value.span)
                    .with_help("write `scores = ...` to give it another name"),
            );
        }
        let held = self.checker.settled(&ty);
        if held.has_params() {
            self.error(shared_of_a_type_parameter(value.span));
        }
        // What a `shared` value holds is kept where every task can reach it, and a generator
        // can't be kept there yet.
        if self.holds_generator(&held, &mut Vec::new()) {
            self.error(
                Diagnostic::new("a `shared` value can't hold a generator yet", value.span)
                    .with_help(crate::launch::GENERATOR_HELP),
            );
        }
        self.checker.shared_values.insert((value.span.start, value.span.end), held.clone());
        self.record(at, &shared(held.clone()));
        self.declare(name, shared(held), false, at);
    }

    /// `lock a, b as ref x, ref y:`: each value is `shared`, and each name is what it holds for
    /// the block, to read, or with `ref` to change.
    pub(crate) fn lock_block(&mut self, value: &Expr, names: &[WithName], body: &[Stmt], used: bool) -> Flow {
        let ty = self.expr(value);
        let (items, types): (Vec<&Expr>, Vec<Type>) = match (&value.kind, ty) {
            (ExprKind::Tuple(items), Type::Tuple(types)) if names.len() != 1 => {
                (items.iter().collect(), types)
            }
            (_, ty) => (vec![value], vec![ty]),
        };
        self.no_value_locked_twice(&items, names);
        if let Some(outer) = self.lock.clone() {
            self.error(
                Diagnostic::new(format!("this `lock` waits for another lock {outer}"), value.span).with_help(
                    "take them in one `lock`, like `lock a, b as ref x, ref y:`, so they can't be taken in opposite orders",
                ),
            );
        }
        let mut inner = Vec::new();
        for (item, ty) in items.iter().zip(&types) {
            if let Some(held) = held(ty) {
                inner.push(held.clone());
                continue;
            }
            if !matches!(ty, Type::Unknown) {
                let message = format!("`lock` takes `shared` values, but this is {}", a(ty));
                self.error(Diagnostic::new(message, item.span).with_help(
                    "make it with `shared name = ...`, or use `with` for what's lent, like a file",
                ));
            }
            inner.push(Type::Unknown);
        }
        let written = fennec_syntax::dotted(value)
            .map_or_else(|| "a lock".to_owned(), |name| format!("`{name}`'s lock"));
        self.checker.note_takes_lock(&self.key.clone(), &written);
        for arrow in self.arrows_open.clone() {
            self.checker.values.locks.entry(arrow).or_insert_with(|| written.clone());
        }
        let shown: Vec<String> = names.iter().map(|held| format!("`{}`", held.name)).collect();
        let outer = self.lock.replace(format!(
            "while {} {} locked",
            shown.join(" and "),
            if shown.len() == 1 { "is" } else { "are" }
        ));
        self.push(false);
        for (held, ty) in names.iter().zip(inner) {
            self.declare_as(&held.name, ty, held.is_mut, Origin::Locked, held.span);
        }
        let flow = self.stmts(body, used);
        self.pop();
        self.lock = outer;
        flow
    }

    /// Refuses a `lock` that names one value twice, by the same name or the same path, as
    /// `lock a, a` or `lock s.x, s.x`: it's one `shared` value, which one `lock` can't take twice.
    /// One the checker can't see is the same, as two items of one list, stops the program instead.
    fn no_value_locked_twice(&mut self, items: &[&Expr], names: &[WithName]) {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for (at, item) in items.iter().enumerate() {
            let Some(path) = fennec_syntax::dotted(item) else { continue };
            let Some(&(_, first)) = seen.iter().find(|(seen, _)| *seen == path) else {
                seen.push((path, at));
                continue;
            };
            let message = format!("this `lock` takes `{path}` twice, but it's one `shared` value");
            let help = match names.get(first) {
                Some(held) => format!(
                    "take it once, as `lock {path} as ref {}:`, and use `{}` for both",
                    held.name, held.name
                ),
                None => format!("take `{path}` once"),
            };
            self.error(Diagnostic::new(message, item.span).with_help(help));
        }
    }

    /// Reports a `with` on a `shared` value, which is reached with `lock`, with the fix that says so.
    pub(crate) fn with_on_shared(&mut self, ty: &Type, span: Span, at: Span, names: &[WithName]) {
        // Lowering writes each `lock` in as a `with` on a `shared` declaration, naming what it
        // holds `{name}__lock`, which a check of what it wrote takes as written.
        let first = !self.checker.again && !lowered(names);
        let locks = |ty: &Type| held(ty).is_some() || (first && self.checker.program.takes_lock(ty));
        let shared = match ty {
            Type::Tuple(types) => types.iter().any(locks),
            ty => locks(ty),
        };
        if shared {
            let keyword = Span { start: at.start, end: at.start + 4 };
            self.error(
                Diagnostic::new("a `shared` value is reached with `lock`, not `with`", span)
                    .with_help("write `lock` in place of `with`: `lock scores as ref current:`")
                    .with_fix(keyword, "lock"),
            );
        }
    }

    /// Reports reaching into a `shared` value outside a `lock`, through a field or a method.
    /// Returns whether it was one, so nothing more is said about it.
    pub(crate) fn outside_lock(&mut self, object: &Expr, ty: &Type, span: Span) -> bool {
        if held(ty).is_none() {
            return false;
        }
        let written = fennec_syntax::dotted(object).unwrap_or_else(|| "it".to_owned());
        let message = format!("`{written}` is `shared`, so it's reached inside a `lock`");
        self.error(Diagnostic::new(message, span).with_help(format!(
            "write `lock {written} as current:` around this to read it, or `lock {written} as ref current:` to change it"
        )));
        true
    }
}
