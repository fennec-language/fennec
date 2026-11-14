//! `shared` declarations: a counted handle to one cell, whose methods lock it. A program no longer
//! writes one, as `shared T` and `lock` replaced them, and one it writes is refused with a fix;
//! lowering writes each `shared T` in as one (see `lock_lowering`), and the rules here hold for
//! what it writes. Assigning a handle gives another name for the same cell. A `shared` value holds
//! plain values and `Id`s, never itself, and none lives at the top of a file.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind, TypeDef, TypeExpr, WithName};

use crate::checker::Checker;
use crate::function::{FnCx, Origin};
use crate::program::Program;
use crate::stmt::Flow;
use crate::types::{Type, a};

/// Whether a type is a capability, through which all I/O goes.
pub(crate) fn is_capability(name: &str) -> bool {
    matches!(
        name,
        "Stdout" | "Stderr" | "Fs" | "Clock" | "Args" | "Stdin" | "Process" | "Entropy" | "Env" | "Signals"
    )
}

/// Whether a type is a capability, by itself.
pub(crate) fn is_capability_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name) if is_capability(name) || name == "Net")
}

impl Program {
    /// Whether a type is `shared`.
    pub(crate) fn is_shared(&self, ty: &Type) -> bool {
        if crate::lock::held(ty).is_some() {
            return true;
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return false };
        self.type_info(name).is_some_and(|info| info.def.shared)
    }

    /// Whether a value of this type has a lock: a `shared` type a program declares. One the
    /// standard library ships, like a connection, is a resource: it manages its own sides, takes
    /// no lock, and a `with` on it closes it when the block ends.
    pub(crate) fn takes_lock(&self, ty: &Type) -> bool {
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return false };
        self.type_info(name).is_some_and(|info| info.def.shared && !info.prelude)
    }
}

impl Checker {
    /// Whether a value of this type holds a capability, in it or anything inside it.
    pub(crate) fn holds_capability(&mut self, ty: &Type, open: &mut Vec<String>) -> bool {
        if is_capability_type(ty) {
            return true;
        }
        if ty.children().iter().any(|child| self.holds_capability(&(*child).clone(), open)) {
            return true;
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return false };
        if open.contains(name) {
            return false;
        }
        let Some(info) = self.program.type_info(name) else { return false };
        let def = info.def.clone();
        open.push(name.clone());
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        let mut found = false;
        for (case, field) in fields.chain(in_cases) {
            let held = self.field_type(name, case, &field.name);
            if self.holds_capability(&held, open) {
                found = true;
                break;
            }
        }
        open.pop();
        found
    }

    /// Whether a value of this type is or holds a `shared` value, as written, `shared T`, or as
    /// lowering declares one, in it or anything inside it.
    pub(crate) fn shows_shared(&mut self, ty: &Type, open: &mut Vec<String>) -> bool {
        if self.program.is_shared(ty) {
            return true;
        }
        if ty.children().iter().any(|child| self.shows_shared(&(*child).clone(), open)) {
            return true;
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return false };
        if open.contains(name) {
            return false;
        }
        let Some(info) = self.program.type_info(name) else { return false };
        let def = info.def.clone();
        open.push(name.clone());
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        let mut found = false;
        for (case, field) in fields.chain(in_cases) {
            let held = self.field_type(name, case, &field.name);
            if self.shows_shared(&held, open) {
                found = true;
                break;
            }
        }
        open.pop();
        found
    }

    /// The `shared` type a value of this type holds, in it or anything inside it.
    pub(crate) fn shared_inside(&mut self, ty: &Type, open: &mut Vec<String>) -> Option<String> {
        if self.program.takes_lock(ty) {
            return ty.type_name().map(str::to_owned);
        }
        for child in ty.children() {
            if let Some(found) = self.shared_inside(&child.clone(), open) {
                return Some(found);
            }
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return None };
        if open.contains(name) {
            return None;
        }
        let info = self.program.type_info(name)?;
        let def = info.def.clone();
        open.push(name.clone());
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        let mut found = None;
        for (case, field) in fields.chain(in_cases) {
            let held = self.field_type(name, case, &field.name);
            found = self.shared_inside(&held, open);
            if found.is_some() {
                break;
            }
        }
        open.pop();
        found
    }

    /// Reports a `def del`, which there isn't: a destructor runs at a moment no line shows and has
    /// nowhere to report a failure, so what needs cleanup is opened with `with` from a source.
    pub(crate) fn check_dels(&mut self) {
        let own: Vec<String> = self
            .program
            .types
            .values()
            .filter(|info| !info.prelude)
            .map(|info| info.def.name.clone())
            .collect();
        for name in own {
            let Some((_, entry)) = self.program.method(&name, "del") else { continue };
            let shown = fennec_syntax::rename::shown_type_name(&name);
            let message = format!("there's no `def del`: nothing runs when a `{shown}` goes away");
            self.diagnostics.push(Diagnostic::new(message, entry.def.span).with_help(format!(
                "open what needs cleanup with `with`, from a source: `def opened() -> with {shown}:` makes one, `lend`s it, and cleans up after the `lend`"
            )));
        }
    }

    /// The `shared` types a value of this type reaches through what it holds, at any depth,
    /// including the type itself when it's `shared`.
    pub(crate) fn shared_reached(&mut self, ty: &Type, open: &mut Vec<String>, found: &mut Vec<String>) {
        for child in ty.children() {
            self.shared_reached(&child.clone(), open, found);
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return };
        if self.program.takes_lock(ty) && !found.contains(name) {
            found.push(name.clone());
        }
        if open.contains(name) {
            return;
        }
        let Some(info) = self.program.type_info(name) else { return };
        let def = info.def.clone();
        open.push(name.clone());
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        for (case, field) in fields.chain(in_cases) {
            let held = self.field_type(name, case, &field.name);
            self.shared_reached(&held, open, found);
        }
        open.pop();
    }

    /// Whether a `shared` value of type `outer` holds one of type `inner`, at any depth: its
    /// lock comes first, so a lock body for `outer` can take `inner`'s.
    pub(crate) fn holds_shared(&mut self, outer: &str, inner: &str) -> bool {
        if outer == inner {
            return false;
        }
        let mut found = Vec::new();
        self.shared_reached(&Type::named(outer), &mut Vec::new(), &mut found);
        found.iter().any(|name| name == inner)
    }

    /// Reports `shared` types that hold each other in a cycle. One can hold another, and its
    /// lock comes first, but a cycle has no first lock.
    pub(crate) fn check_shared_types(&mut self) {
        let defs: Vec<_> = self
            .program
            .types
            .values()
            .filter(|info| info.def.shared && !info.prelude)
            .map(|info| info.def.clone())
            .collect();
        // Lowering writes in a `shared` declaration for each `shared T`, with no place in the
        // source, which a check of what it wrote takes as written.
        if !self.again {
            for def in defs.iter().filter(|def| def.span != Span::default()) {
                let refused = self.declared_shared(def);
                self.diagnostics.push(refused);
            }
        }
        for def in defs {
            for field in &def.fields {
                let held = self.field_type(&def.name, None, &field.name);
                let mut found = Vec::new();
                self.shared_reached(&held, &mut Vec::new(), &mut found);
                if !found.contains(&def.name) {
                    continue;
                }
                let first = self.shared_inside(&held, &mut Vec::new()).unwrap_or_else(|| def.name.clone());
                let message = if first == def.name {
                    format!(
                        "`{}` holds itself, and `shared` types can't hold each other in a cycle",
                        def.name
                    )
                } else {
                    format!(
                        "`{}` holds `{first}`, which holds `{}` again, and `shared` types can't hold each other in a cycle",
                        def.name, def.name
                    )
                };
                self.diagnostics.push(Diagnostic::new(message, field.span).with_help(
                    "a cycle has no lock to take first: hold an `Id` on one side, or keep the other handle where both are used",
                ));
            }
        }
        self.shared_cycles();
    }

    /// Reports each type whose fields reach a `shared` value of its own type, at any depth: a
    /// `shared` value is freed when the last holder lets go, and one that holds itself never is.
    fn shared_cycles(&mut self) {
        let defs: Vec<_> = self
            .program
            .types
            .values()
            .filter(|info| !info.def.shared && !info.prelude)
            .map(|info| info.def.clone())
            .collect();
        for def in defs {
            let fields = def.fields.iter().map(|field| (None, field));
            let in_cases = def
                .cases
                .iter()
                .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
            for (case, field) in fields.chain(in_cases) {
                let ty = self.field_type(&def.name, case, &field.name);
                let mut found = Vec::new();
                self.shared_held(&ty, &mut Vec::new(), &mut found);
                if !found.contains(&def.name) {
                    continue;
                }
                let name = fennec_syntax::rename::shown_type_name(&def.name);
                let message = match found.first().map(|first| fennec_syntax::rename::shown_type_name(first)) {
                    Some(first) if first != name => format!(
                        "a `shared {name}` holds a `shared {first}`, which holds a `shared {name}` again, and `shared` values can't hold each other in a cycle"
                    ),
                    _ => format!(
                        "a `shared {name}` can hold itself, and `shared` values can't hold each other in a cycle"
                    ),
                };
                self.diagnostics.push(Diagnostic::new(message, field.span).with_help(
                    "a `shared` value is freed when the last holder lets go, and one in a cycle never is: hold an `Id` from a `Table` on one side",
                ));
            }
        }
    }

    /// The types of the `shared` values a value of this type reaches through what it holds, at
    /// any depth, in the order they're met.
    fn shared_held(&mut self, ty: &Type, open: &mut Vec<String>, found: &mut Vec<String>) {
        if let Some(name) = crate::lock::held(ty).and_then(Type::type_name)
            && !found.iter().any(|seen| seen == name)
        {
            found.push(name.to_owned());
        }
        for child in ty.children() {
            self.shared_held(&child.clone(), open, found);
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return };
        if open.contains(name) {
            return;
        }
        let Some(info) = self.program.type_info(name) else { return };
        let def = info.def.clone();
        open.push(name.clone());
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        for (case, field) in fields.chain(in_cases) {
            let held = self.field_type(name, case, &field.name);
            self.shared_held(&held, open, found);
        }
        open.pop();
    }
}

impl Checker {
    /// The refusal of a `shared Name:` declaration, which `shared T` replaced. Its fix makes
    /// what's mechanical: `struct Name:`, `shared Name` wherever the type is written, and
    /// `shared x = Name(...)` where a name is given a new one. What's left, like a method called
    /// outside a `lock`, is refused once it's made, with how to write it.
    fn declared_shared(&self, def: &TypeDef) -> Diagnostic {
        let name = &def.name;
        let keyword = Span { start: def.span.start, end: def.span.start + 6 };
        let mut written = Vec::new();
        let mut made = Vec::new();
        for info in self.program.types.values().filter(|info| !info.prelude) {
            let fields = info.def.fields.iter().chain(info.def.cases.iter().flat_map(|case| &case.fields));
            for ty in fields.filter_map(|field| field.ty.as_ref()) {
                types_named(ty, name, &mut written);
            }
        }
        for entry in self.program.functions.values().filter(|entry| !entry.prelude) {
            let def = &entry.def;
            let signature = def.params.iter().filter_map(|param| param.ty.as_ref()).chain(&def.ret);
            for ty in signature {
                types_named(ty, name, &mut written);
            }
            in_stmts(&def.body, name, &mut written, &mut made);
        }
        for test in &self.program.tests {
            in_stmts(&test.body, name, &mut written, &mut made);
        }
        let mut refused = Diagnostic::new(
            format!("`shared {}:` is gone: a type is declared with `struct`, and `shared` goes where a value is shared", shown(name)),
            keyword,
        )
        .with_help(format!(
            "declare `struct {0}:`, write `shared {0}` for the type of a shared value, and make one with `shared x = {0}()`",
            shown(name)
        ))
        .with_fix(keyword, "struct");
        for at in written.into_iter().chain(made) {
            refused = refused.with_fix(Span { start: at, end: at }, "shared ");
        }
        refused
    }
}

/// The type's name as it's written, which a type named like one of std's isn't checked under.
fn shown(name: &str) -> &str {
    fennec_syntax::rename::shown_type_name(name)
}

/// Where each `name` starts in `ty`, as written.
fn types_named(ty: &TypeExpr, name: &str, found: &mut Vec<u32>) {
    if ty.name == name && ty.span != Span::default() {
        found.push(ty.span.start);
    }
    for arg in &ty.args {
        types_named(arg, name, found);
    }
}

/// The types named `name` written in `body`, and where each `x = Name(...)` that gives a name its
/// value starts.
fn in_stmts(body: &[Stmt], name: &str, written: &mut Vec<u32>, made: &mut Vec<u32>) {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Assign { target, op: None, value, declare_mut: false, ty } => {
                if let Some(ty) = ty {
                    types_named(ty, name, written);
                }
                let made_here = matches!(&value.kind, ExprKind::Call { callee, .. }
                    if matches!(&callee.kind, ExprKind::Name(callee) if callee == name));
                if ty.is_none() && made_here && matches!(target.kind, ExprKind::Name(_)) {
                    made.push(target.span.start);
                }
            }
            StmtKind::Assign { ty: Some(ty), .. } => types_named(ty, name, written),
            StmtKind::If { branches, otherwise } => {
                for (_, block) in branches {
                    in_stmts(block, name, written, made);
                }
                in_stmts(otherwise.as_deref().unwrap_or_default(), name, written, made);
            }
            StmtKind::While { body: block, .. }
            | StmtKind::For { body: block, .. }
            | StmtKind::With { body: block, .. } => in_stmts(block, name, written, made),
            StmtKind::OnFailure { cleanup, body: block, .. } => {
                in_stmts(cleanup, name, written, made);
                in_stmts(block, name, written, made);
            }
            _ => {}
        }
    }
}

/// Why a lock body can't hand on what holds a handle, and what to do instead.
const RETAKES: &str =
    "the call could lock the handle again: do that work here, through the locked name, or after the block";

impl FnCx<'_> {
    /// `with value as name:`, or `with a, b as mut x, mut y:`. Several values are `shared`
    /// handles locked together, and a handle is always named: the block changes it through that
    /// name, which holds its lock, to write when it's `mut`.
    pub(crate) fn with_block(
        &mut self,
        value: &Expr,
        names: &[WithName],
        body: &[Stmt],
        used: bool,
        at: Span,
    ) -> Flow {
        let ty = self.with_value(value);
        self.with_on_shared(&ty, value.span, at, names);
        let (items, types) = match (&value.kind, ty) {
            (ExprKind::Tuple(items), Type::Tuple(types)) if names.len() != 1 => {
                (items.iter().collect(), types)
            }
            (_, ty) => (vec![value], vec![ty]),
        };
        self.several_held(value, &items, &types);
        for (item, ty) in items.iter().zip(&types) {
            let shared = self.checker.program.takes_lock(ty);
            if shared && names.is_empty() {
                let written = fennec_syntax::dotted(item).unwrap_or_else(|| "it".to_owned());
                self.error(
                    Diagnostic::new(format!("this `with` locks {}, so it needs a name for it", a(ty)), item.span)
                        .with_help(format!(
                            "name it after `as`, like `with {written} as ref locked:`, and change it through that name"
                        )),
                );
            }
        }
        // A `lock` lowering wrote in was held to `lock`'s own rules when it was checked as
        // written, so the rules for a `with` on a `shared` declaration's value don't apply to it.
        let lowered = crate::lock::lowered(names);
        let locked: Vec<&WithName> = names
            .iter()
            .zip(&types)
            .filter(|(_, ty)| !lowered && self.checker.program.takes_lock(ty))
            .map(|(held, _)| held)
            .collect();
        let locking: Vec<Type> =
            types.iter().filter(|ty| !lowered && self.checker.program.takes_lock(ty)).cloned().collect();
        let inner = locking.iter().all(|ty| self.takes_inner(ty));
        if let (Some(held), Some(first), false) = (
            self.lock.clone(),
            items
                .iter()
                .find(|item| self.recorded(item.span).is_some_and(|ty| self.checker.program.takes_lock(&ty))),
            inner,
        ) {
            self.error(
                Diagnostic::new(format!("this `with` takes another lock {held}"), first.span).with_help(
                    "a lock body takes only the locks of `shared` values its locked ones hold: lock these together in one `with`, like `with a as ref x, b as ref y:`",
                ),
            );
        }
        let outer_types = self.locked_types.clone();
        let outer = if locked.is_empty() {
            self.lock.clone()
        } else {
            let newly = types.iter().filter(|ty| self.checker.program.takes_lock(ty));
            self.locked_types.extend(newly.filter_map(|ty| ty.type_name().map(str::to_owned)));
            let names: Vec<String> = locked.iter().map(|held| format!("`{}`", held.name)).collect();
            let (names, verb) = match names.as_slice() {
                [one] => (one.clone(), "is"),
                [rest @ .., last] => (format!("{} and {last}", rest.join(", ")), "are"),
                [] => (String::new(), "is"),
            };
            self.lock.replace(format!("while {names} {verb} locked"))
        };
        self.push(false);
        if let ([value], [_, _, ..]) = (items.as_slice(), names) {
            self.split_lent(value, types.first().unwrap_or(&Type::Unknown), names, body);
            let flow = self.stmts(body, used);
            self.pop();
            self.lock = outer;
            self.locked_types = outer_types;
            return flow;
        }
        for (held, ty) in names.iter().zip(types) {
            let origin = if self.checker.program.takes_lock(&ty) { Origin::Locked } else { Origin::Local };
            // What a source lends is the block's to use, `ref self` methods included: it only
            // can't leave the block.
            let is_mut = held.is_mut || crate::lend::lent(&ty).is_some();
            let ty = self.lent_to(ty, body);
            self.declare_as(&held.name, ty, is_mut, origin, held.span);
        }
        let flow = self.stmts(body, used);
        self.pop();
        self.lock = outer;
        self.locked_types = outer_types;
        flow
    }

    /// Several values in one `with`: handles opened in order and finished innermost first, as
    /// one `with` inside another, which lowering writes them as, or `shared` handles locked
    /// together, which don't mix with them.
    fn several_held(&mut self, value: &Expr, items: &[&Expr], types: &[Type]) {
        if items.len() < 2 {
            return;
        }
        if !types.iter().any(|ty| self.checker.program.is_shared(ty)) {
            self.checker.opened_apart.insert((value.span.start, value.span.end));
            return;
        }
        for (item, ty) in items.iter().zip(types) {
            if !self.checker.program.is_shared(ty) && *ty != Type::Unknown {
                let message = format!("this `with` locks `shared` handles, so it can't open {} too", a(ty));
                self.error(
                    Diagnostic::new(message, item.span).with_help(
                        "open it in a `with` of its own, and lock the handles with `lock` inside it",
                    ),
                );
            }
        }
    }

    /// `with conn.split() as reader, writer:`: a source that lends a tuple, each item named.
    fn split_lent(&mut self, value: &Expr, ty: &Type, names: &[WithName], body: &[Stmt]) {
        let lent = crate::lend::lent(ty).cloned();
        let items = match self.lent_to(ty.clone(), body) {
            Type::Tuple(items) if lent.is_some() && items.len() == names.len() => items,
            Type::Unknown => vec![Type::Unknown; names.len()],
            other => {
                let what =
                    if lent.is_some() { format!("lends {}", a(&other)) } else { format!("is {}", a(&other)) };
                self.error(
                    Diagnostic::new(
                        format!("this `with` names {} values, but what it takes {what}", names.len()),
                        value.span,
                    )
                    .with_help("several names split what a source lends as a tuple of that many: `-> with (Reader, Writer)`"),
                );
                vec![Type::Unknown; names.len()]
            }
        };
        for (held, ty) in names.iter().zip(items) {
            self.declare_as(&held.name, ty, true, Origin::Local, held.span);
        }
    }

    /// Whether every lock held is on a `shared` type that holds one of type `ty`, so its lock
    /// can be taken now: locks go outer type first, which no two lock bodies can take in
    /// opposite orders.
    pub(crate) fn takes_inner(&mut self, ty: &Type) -> bool {
        let Some(name) = ty.type_name().map(str::to_owned) else { return false };
        let held = self.locked_types.clone();
        !held.is_empty() && held.iter().all(|outer| self.checker.holds_shared(outer, &name))
    }

    /// Whether every `shared` type a value of type `ty` holds is an inner one, as `takes_inner`.
    fn holds_only_inner(&mut self, ty: &Type) -> bool {
        let mut found = Vec::new();
        self.checker.shared_reached(ty, &mut Vec::new(), &mut found);
        found.iter().all(|name| self.takes_inner(&Type::named(name)))
    }

    /// Whether an expression is a name that holds its handle's lock already: `self` in a
    /// `shared` type's method, or a name a `with` block gave a handle it locked.
    fn holds_lock(&self, expr: &Expr) -> bool {
        matches!(&expr.kind, ExprKind::Name(name) if name == "self" || self.origin(name) == Some(Origin::Locked))
    }

    /// Reports taking a `shared` handle's lock inside a lock body, which could wait forever on a
    /// lock another task holds while it waits for this one. `what` takes it: "calling `add`".
    /// Reports a value holding a `shared` one shown or compared inside a lock body: printed, in an
    /// f-string, by `str`, by a failed `assert` or with `==`. Either reads what the `shared` value
    /// holds under its lock, which a lock body can't take, as one its own thread holds would never
    /// come free.
    pub(crate) fn shown_in_lock(&mut self, ty: &Type, how: &str, at: Span) -> bool {
        let Some(held) = self.lock.clone() else { return false };
        let ty = self.checker.settled(ty);
        if !self.checker.shows_shared(&ty, &mut Vec::new()) {
            return false;
        }
        self.error(
            Diagnostic::new(format!("{how} would take the lock of the `shared` value in it {held}"), at)
                .with_help("a `shared` value is shown and compared under its lock, which a lock body can't take again: do it after the block, or through the name the lock gives"),
        );
        true
    }

    /// `==` on a `shared` value compares what it holds, under its lock, which a lock body can't
    /// take: said once for the comparison.
    pub(crate) fn compared_in_lock(&mut self, (left, right): (&Type, &Type), spans: (Span, Span)) {
        if !self.shown_in_lock(left, "comparing this", spans.0) {
            let _ = self.shown_in_lock(right, "comparing this", spans.1);
        }
    }

    /// A failed `assert` that compares shows each side, which inside a lock body can't hold a
    /// `shared` value. `==` and `!=` are refused there already, as they compare what one holds.
    pub(crate) fn assert_shown_in_lock(&mut self, cond: &Expr) {
        if self.lock.is_none() {
            return;
        }
        let ExprKind::Binary { op, lhs, rhs } = &cond.kind else { return };
        if matches!(op, fennec_syntax::BinOp::Eq | fennec_syntax::BinOp::NotEq) {
            return;
        }
        for side in [lhs, rhs] {
            let ty = self.peek_type(side);
            if self.shown_in_lock(&ty, "this `assert`", side.span) {
                return;
            }
        }
    }

    pub(crate) fn locks_again(&mut self, object: &Expr, ty: &Type, what: &str) {
        let Some(held) = self.lock.clone() else { return };
        if !self.checker.program.takes_lock(ty) || self.holds_lock(object) || self.takes_inner(ty) {
            return;
        }
        let handle = fennec_syntax::dotted(object).unwrap_or_else(|| "a handle".to_owned());
        self.error(Diagnostic::new(format!("{what} locks `{handle}` {held}"), object.span).with_help(
            "a lock body takes only the locks of `shared` values its locked ones hold: lock these together in one `with`, like `with a as ref x, b as ref y:`, and use the names it gives",
        ));
    }

    /// Reports a `shared` handle passed to a call inside a lock body, which could lock it again:
    /// on its own, inside a value or a list, or captured by a function.
    pub(crate) fn handle_passed(&mut self, arg: &Expr, ty: &Type) {
        let Some(held) = self.lock.clone() else { return };
        if self.is_handle_closure(arg) {
            let name = fennec_syntax::dotted(arg).unwrap_or_default();
            self.error(
                Diagnostic::new(
                    format!("`{name}` holds a `shared` handle, so it can't be passed to a call {held}"),
                    arg.span,
                )
                .with_help(RETAKES),
            );
            return;
        }
        if self.holds_only_inner(ty) {
            return;
        }
        if !self.checker.program.takes_lock(ty) {
            if let Some(inner) = self.checker.shared_inside(ty, &mut Vec::new()) {
                let message =
                    format!("{} holds a `shared` `{inner}`, so it can't be passed to a call {held}", a(ty));
                self.error(Diagnostic::new(message, arg.span).with_help(RETAKES));
            }
            return;
        }
        self.error(
            Diagnostic::new(format!("a `shared` handle can't be passed to a call {held}"), arg.span)
                .with_help("the call could lock it again: do that work here, through the locked name, or after the block"),
        );
    }

    /// Reports a method called on a value that holds a `shared` handle inside a lock body, since
    /// the method could lock it. `object` itself being a handle is `locks_again`'s.
    pub(crate) fn method_on_holder(&mut self, object: &Expr, ty: &Type, method: &str) {
        let Some(held) = self.lock.clone() else { return };
        if self.checker.program.takes_lock(ty) || self.holds_only_inner(ty) {
            return;
        }
        if let Some(inner) = self.checker.shared_inside(ty, &mut Vec::new()) {
            let message = format!("calling `{method}` on {} that holds a `shared` `{inner}` {held}", a(ty));
            self.error(Diagnostic::new(message, object.span).with_help(RETAKES));
        }
    }

    /// Remembers a function that captures a `shared` handle, by its name: an arrow function given
    /// to a name, or a nested function. `reads` is what its body reads.
    pub(crate) fn note_closure(&mut self, name: &str, reads: &[String], own: &[String]) {
        let holds = reads.iter().filter(|read| !own.contains(read)).any(|read| {
            self.handle_closures.contains(read)
                || self
                    .lookup(read)
                    .is_some_and(|(ty, _)| self.checker.shared_inside(&ty, &mut Vec::new()).is_some())
        });
        if holds {
            self.handle_closures.insert(name.to_owned());
        }
    }

    fn is_handle_closure(&self, expr: &Expr) -> bool {
        matches!(&expr.kind, ExprKind::Name(name) if self.handle_closures.contains(name))
    }

    /// Reports a call inside a lock body to a function that captures a `shared` handle.
    pub(crate) fn closure_locks_again(&mut self, callee: &Expr) {
        let Some(held) = self.lock.clone() else { return };
        if self.is_handle_closure(callee) {
            let name = fennec_syntax::dotted(callee).unwrap_or_default();
            let message = format!("calling `{name}`, which captures a `shared` handle, could lock it {held}");
            self.error(Diagnostic::new(message, callee.span).with_help(RETAKES));
        }
    }

    /// Reports what a lock body can't do: pause with `yield`, which would hold the lock for as
    /// long as whatever asks for the next item waits. Leaving early with `?`, `fail`, `return`,
    /// `break` or `continue` releases the lock on the way out, and I/O only makes it slow.
    pub(crate) fn not_under_lock(&mut self, what: &str, span: Span, help: &str) {
        if let Some(held) = self.lock.clone() {
            self.error(Diagnostic::new(format!("{what} {held}"), span).with_help(help));
        }
    }

    /// The `shared` value a place changes through without one of its methods, which would skip
    /// its lock: `cache._pages` outside `Cache`'s methods. Its own methods change it through
    /// `self`.
    pub(crate) fn through_shared(&mut self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Name(name) if name != "self" && self.origin(name) != Some(Origin::Locked) => {
                let (ty, _) = self.lookup(name)?;
                self.checker.program.is_shared(&ty).then(|| name.clone())
            }
            ExprKind::Attr { object, name, .. } => {
                if let Some(found) = self.through_shared(object) {
                    return Some(found);
                }
                let owner = self.declared_type_of(object)?;
                let field = self.checker.field_type(owner.type_name()?, None, name);
                self.checker
                    .program
                    .is_shared(&field)
                    .then(|| fennec_syntax::dotted(expr).unwrap_or_default())
            }
            ExprKind::Index { object, .. } => self.through_shared(object),
            _ => None,
        }
    }

    /// The type of a chain of names and fields, from their declarations.
    pub(crate) fn declared_type_of(&mut self, expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Name(name) => self.lookup(name).map(|(ty, _)| ty),
            ExprKind::Attr { object, name, .. } => {
                let owner = self.declared_type_of(object)?;
                Some(self.checker.field_type(owner.type_name()?, None, name))
            }
            _ => None,
        }
    }
}
