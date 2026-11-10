//! A collection can't change while a loop goes over it. Inside `for x in xs:`, nothing may
//! append to `xs`, remove from it, set one of its items, write through it with another `for mut`,
//! or pass it as `mut`, and neither may a function the body calls: each function's summary says
//! which places under its `mut` parameters it changes. The loop goes over a value made first,
//! `before = xs`, instead, or collects the changes and makes them after.
//!
//! A `ref` argument has its place to itself the same way: nothing else in the call can reach it,
//! not even another item of the same collection, as `f(ref xs[0], ref xs[2])` would.
//! Nor can an argument change what its call changes, like `self.items.append(self.grow())` where
//! `grow` appends to `self.items`: the engines would disagree on which change wins.

use std::collections::HashMap;

use fennec_syntax::{Arg, CompKind, Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind};

use crate::Body;
use crate::builtins;
use crate::checker::Checker;
use crate::types::Type;

/// A step along a place: a field, or an index, which could be any item.
type Step = Option<String>;

/// A place: a name and the steps after it, like `xs[i].hp`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Place {
    root: String,
    steps: Vec<Step>,
}

impl Place {
    /// Whether changing one can change the other: one is inside the other, where an index
    /// matches any item and different fields are different places.
    pub(crate) fn overlaps(&self, other: &Place) -> bool {
        self.root == other.root
            && self.steps.iter().zip(&other.steps).all(|(a, b)| a.is_none() || b.is_none() || a == b)
    }

    /// This place, with `inner` read as a place under it.
    fn under(&self, inner: &[Step]) -> Place {
        Place { root: self.root.clone(), steps: self.steps.iter().chain(inner).cloned().collect() }
    }

    /// The place as it's written, with `[...]` for any index.
    fn shown(&self) -> String {
        self.steps.iter().fold(self.root.clone(), |text, step| match step {
            Some(field) => format!("{text}.{field}"),
            None => format!("{text}[...]"),
        })
    }
}

/// The place an expression names, if it names one.
pub(crate) fn place(expr: &Expr) -> Option<Place> {
    match &expr.kind {
        ExprKind::Name(name) => Some(Place { root: name.clone(), steps: Vec::new() }),
        ExprKind::Attr { object, name, optional: false } => {
            let mut found = place(object)?;
            found.steps.push(Some(name.clone()));
            Some(found)
        }
        ExprKind::Index { object, .. } => {
            let mut found = place(object)?;
            found.steps.push(None);
            Some(found)
        }
        _ => None,
    }
}

/// The collections a loop goes over: `xs` in `for x in xs`, also through `enumerate(xs)`,
/// `reversed(xs)`, `zip(xs, ys)` and a dict's `keys()`, `values()` and `items()`. A slice is
/// a copy, so a loop over one goes over nothing that can change.
fn looped_over(iter: &Expr) -> Vec<(Place, &Expr)> {
    match &iter.kind {
        ExprKind::Call { callee, args } => match &callee.kind {
            ExprKind::Name(name) if matches!(name.as_str(), "enumerate" | "reversed" | "zip") => {
                args.iter().filter(|arg| arg.name.is_none()).flat_map(|arg| looped_over(&arg.value)).collect()
            }
            ExprKind::Attr { object, name, .. }
                if args.is_empty() && matches!(name.as_str(), "keys" | "values" | "items") =>
            {
                looped_over(object)
            }
            _ => Vec::new(),
        },
        _ => place(iter).map(|found| vec![(found, iter)]).unwrap_or_default(),
    }
}

/// What a function changes under each of its `mut` parameters: the places relative to the
/// parameter, where an empty one is the whole parameter.
type Summary = HashMap<String, Vec<Vec<Step>>>;

/// The summaries worked out for good, kept from one function's check to the next.
#[derive(Default)]
pub(crate) struct Settled(HashMap<String, Summary>);

struct Loops<'c, 's> {
    checker: &'c Checker,
    settled: &'s mut Settled,
    /// What's known so far of the functions whose summaries aren't settled yet.
    summaries: HashMap<String, Summary>,
    /// Those functions, in the order they were reached, each with where it is in `open`.
    open: Vec<String>,
    places: HashMap<String, usize>,
    /// The earliest place in `open` the function being worked out calls back into, directly or
    /// through what it calls.
    lowest: usize,
    /// How many calls have come back into a function not settled yet.
    calls_back: usize,
    problems: Vec<Diagnostic>,
}

/// Checks a function's loops, reporting each change to what a loop goes over.
pub(crate) fn check_function(
    checker: &Checker,
    settled: &mut Settled,
    key: &str,
    def: &FnDef,
) -> Vec<Diagnostic> {
    let mut loops = Loops {
        checker,
        settled,
        summaries: HashMap::new(),
        open: Vec::new(),
        places: HashMap::new(),
        lowest: usize::MAX,
        calls_back: 0,
        problems: Vec::new(),
    };
    loops.loops_in(&def.body, checker.typed.get(key));
    loops.local_refs_in(&def.body, checker.typed.get(key));
    loops.arguments_changing_their_call(&def.body, checker.typed.get(key));
    loops.generators_in(&def.body, checker.typed.get(key));
    loops.problems
}

impl Loops<'_, '_> {
    /// Checks every loop in a block, and the loops inside them.
    fn loops_in(&mut self, stmts: &[Stmt], body: Option<&Body>) {
        for stmt in stmts {
            if let StmtKind::For { iter, is_mut, body: block, .. } = &stmt.kind {
                let changes = self.changes_in(block, body);
                for (looped, written) in looped_over(iter) {
                    let shown = fennec_syntax::dotted(written).unwrap_or_else(|| "it".to_owned());
                    // A `for mut` over a copy would write to the copy.
                    let help = if *is_mut {
                        "loop over indexes, or collect the changes and make them after the loop"
                    } else {
                        "use `for ref`, or loop over indexes"
                    };
                    for (_, span) in changes.iter().filter(|(changed, _)| changed.overlaps(&looped)) {
                        let message = format!("you can't change `{shown}` while looping over it");
                        self.problems.push(
                            Diagnostic::new(message, *span)
                                .with_help(help)
                                .also_at_the(fennec_syntax::Also::Loop, written.span),
                        );
                    }
                }
            }
            for block in blocks(stmt) {
                self.loops_in(block, body);
            }
        }
    }

    /// Reports each change to what a `ref name = place` names, made another way while the name
    /// is in use: from the `ref` to the last statement in its block that uses it.
    fn local_refs_in(&mut self, stmts: &[Stmt], body: Option<&Body>) {
        for (at, stmt) in stmts.iter().enumerate() {
            if let Some((name, held)) = stmt.kind.local_ref()
                && let Some(named) = place(held)
            {
                let rest = stmts.get(at + 1..).unwrap_or_default();
                let alive = rest
                    .iter()
                    .rposition(|later| mentions(later, name))
                    .map_or(&[][..], |last| rest.get(..=last).unwrap_or_default());
                let shown = place_text(held);
                let mut refs = Vec::new();
                refs_in(alive, &mut refs);
                for (changed, span) in self.changes_in(alive, body) {
                    if !changed.overlaps(&named) {
                        continue;
                    }
                    let problem = match refs.iter().find(|(_, value)| *value == span) {
                        Some((other, _)) => Diagnostic::new(
                            format!("`{other}` is a second `ref` into `{}` while `{name}` is still in use", changed.shown()),
                            span,
                        )
                        .with_help(format!("one `ref` into a place at a time: declare `{other}` after the last line that uses `{name}`")),
                        None => Diagnostic::new(
                            format!("this changes `{}` while `{name}` is a `ref` to `{shown}`", changed.shown()),
                            span,
                        )
                        .with_help(format!("change it through `{name}`, or after the last line that uses `{name}`")),
                    };
                    self.problems.push(problem);
                }
            }
            for block in blocks(stmt) {
                self.local_refs_in(block, body);
            }
            for value in exprs(stmt) {
                fennec_syntax::read::in_expr(value, &mut |inner| {
                    if let ExprKind::Match { arms, .. } = &inner.kind {
                        for arm in arms {
                            self.local_refs_in(&arm.body, body);
                        }
                    }
                });
            }
        }
    }

    /// Reports each call inside an argument that changes a place the call it's passed to also
    /// changes: what a method changing its value is called on, or a `ref` argument's place.
    fn arguments_changing_their_call(&mut self, stmts: &[Stmt], body: Option<&Body>) {
        for stmt in stmts {
            for expr in exprs(stmt) {
                self.argument_changes_in(expr, body);
            }
            for block in blocks(stmt) {
                self.arguments_changing_their_call(block, body);
            }
        }
    }

    fn argument_changes_in(&mut self, expr: &Expr, body: Option<&Body>) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            self.receiver_given_again(callee, args, body);
            let held = self.call_changes(callee, args, body);
            if !held.is_empty() {
                for arg in args {
                    let mut changed = Vec::new();
                    self.written_changes(&arg.value, body, &mut changed);
                    for (place, written, span) in changed {
                        let Some((both, _)) = held.iter().find(|(held, held_written)| {
                            held.overlaps(&place) && !written_apart(held_written, written)
                        }) else {
                            continue;
                        };
                        let message = format!(
                            "this changes `{}`, which the call it's passed to changes too",
                            both.shown()
                        );
                        self.problems.push(Diagnostic::new(message, span).with_help(ARGUMENT_CHANGES_FIX));
                    }
                }
            }
        }
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                self.arguments_changing_their_call(&arm.body, body);
            }
        }
        for inner in crate::effects::children(expr) {
            self.argument_changes_in(inner, body);
        }
    }

    /// Reports each argument that reaches a part of its receiver the `ref self` method changes,
    /// as `p.change(p)` and `b.grab(b.items)` would: the method can change or free what the
    /// argument is read from. A number is passed as its value, so `counter.add(counter.step)` is
    /// fine, and `unit.keep(unit.home)` is when `keep` leaves `home` alone.
    fn receiver_given_again(&mut self, callee: &Expr, args: &[Arg], body: Option<&Body>) {
        let ExprKind::Attr { object, name, .. } = &callee.kind else { return };
        let Some(called_on) = place(object) else { return };
        let Some(body) = body else { return };
        let Some(owner) = body.expr(object.span).and_then(Type::type_name) else { return };
        let Some((key, entry)) = self.checker.program.method(owner, name) else { return };
        if !entry.def.params.first().is_some_and(|param| param.is_self && param.is_mut) {
            return;
        }
        let changes = self.summary(&key).remove("self").unwrap_or_default();
        for arg in args.iter().filter(|arg| !arg.is_mut && !passed_as_a_value(body.expr(arg.value.span))) {
            let Some(given) = place(&arg.value) else { continue };
            if changes.iter().any(|inner| called_on.under(inner).overlaps(&given))
                && !written_apart(&arg.value, object)
            {
                let (first, second) = (written(&arg.value, false), written(object, false));
                let message = format!("{first} overlaps {second}, which this method changes");
                self.problems.push(Diagnostic::new(message, arg.value.span).with_help(RECEIVER_FIX));
            }
        }
    }

    /// Reports each generator expression, like `sum(bump(ref count) for x in xs)`, that changes a
    /// name around it. Its items are made as they're asked for, so when each change happens would
    /// depend on what takes them: a generator only reads.
    fn generators_in(&mut self, stmts: &[Stmt], body: Option<&Body>) {
        for stmt in stmts {
            for expr in exprs(stmt) {
                self.generators_in_expr(expr, body);
            }
            for block in blocks(stmt) {
                self.generators_in(block, body);
            }
            if let StmtKind::Lend { otherwise, .. } = &stmt.kind {
                self.generators_in(otherwise, body);
            }
        }
    }

    fn generators_in_expr(&mut self, expr: &Expr, body: Option<&Body>) {
        if let ExprKind::Comprehension { kind: CompKind::Generator, elem, value, names, cond, .. } =
            &expr.kind
        {
            let mut changed = Vec::new();
            for part in std::iter::once(elem.as_ref()).chain(value.as_deref()).chain(cond.as_deref()) {
                self.written_changes(part, body, &mut changed);
                given_as_ref(part, &mut changed);
            }
            let around = changed.into_iter().find(|(place, _, _)| !names.contains(&place.root));
            if let Some((place, _, span)) = around {
                let message = format!("this generator changes `{}`, a name around it", place.root);
                self.problems.push(
                    Diagnostic::new(message, span).with_help(
                        "a generator only reads, since its items are made as they're asked for: change it in a loop",
                    ),
                );
            }
        }
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                self.generators_in(&arm.body, body);
            }
        }
        for inner in crate::effects::children(expr) {
            self.generators_in_expr(inner, body);
        }
    }

    /// The places a call itself changes, apart from what its arguments do, each with the place
    /// as it's written: what it's called on, for a method that changes its value, and what it
    /// changes under each `ref` argument.
    fn call_changes<'e>(
        &mut self,
        callee: &'e Expr,
        args: &'e [Arg],
        body: Option<&Body>,
    ) -> Vec<(Place, &'e Expr)> {
        let mut held = Vec::new();
        if let ExprKind::Attr { object, name, .. } = &callee.kind
            && let Some(receiver) = place(object)
        {
            for inner in self.method_changes(object, name, body) {
                held.push((receiver.under(&inner), object.as_ref()));
            }
        }
        let callee_key = self.function_key(callee, body);
        for (at, arg) in args.iter().enumerate() {
            let Some(given) = place(&arg.value).filter(|_| arg.is_mut) else { continue };
            let inner = callee_key.as_ref().and_then(|key| self.param_changes(key, at, arg.name.as_ref()));
            for inner in inner.unwrap_or_else(|| vec![Vec::new()]) {
                held.push((given.under(&inner), &arg.value));
            }
        }
        held
    }

    /// What the calls in an expression change, as `call_changes` gives it, with where each call is.
    fn written_changes<'e>(
        &mut self,
        expr: &'e Expr,
        body: Option<&Body>,
        found: &mut Vec<(Place, &'e Expr, Span)>,
    ) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            found.extend(
                self.call_changes(callee, args, body)
                    .into_iter()
                    .map(|(place, written)| (place, written, expr.span)),
            );
        }
        for inner in crate::effects::children(expr) {
            self.written_changes(inner, body, found);
        }
    }

    /// Every place a block changes, with where, not counting a name given a new value.
    fn changes_in(&mut self, stmts: &[Stmt], body: Option<&Body>) -> Vec<(Place, Span)> {
        let mut found = Vec::new();
        for stmt in stmts {
            if let StmtKind::Assign { target, .. } = &stmt.kind {
                let targets = match &target.kind {
                    ExprKind::Tuple(items) => items.iter().collect(),
                    _ => vec![target],
                };
                for target in targets {
                    if let Some(changed) = place(target).filter(|_| !matches!(target.kind, ExprKind::Name(_)))
                    {
                        found.push((changed, target.span));
                    }
                }
            }
            // A `ref` can change what it names.
            if let Some((_, held)) = stmt.kind.local_ref()
                && let Some(named) = place(held)
            {
                found.push((named, held.span));
            }
            // A `for mut` writes each item back, whatever its body does with it.
            if let StmtKind::For { iter, is_mut: true, .. } = &stmt.kind {
                for (looped, written) in looped_over(iter) {
                    found.push((looped.under(&[None]), written.span));
                }
            }
            for expr in exprs(stmt) {
                self.calls_in(expr, body, &mut found);
            }
            for block in blocks(stmt) {
                found.extend(self.changes_in(block, body));
            }
        }
        found
    }

    /// The places the calls in an expression change.
    fn calls_in(&mut self, expr: &Expr, body: Option<&Body>, found: &mut Vec<(Place, Span)>) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            if let ExprKind::Attr { object, name, .. } = &callee.kind
                && let Some(receiver) = place(object)
            {
                for inner in self.method_changes(object, name, body) {
                    found.push((receiver.under(&inner), expr.span));
                }
            }
            let callee_key = self.function_key(callee, body);
            for (at, arg) in args.iter().enumerate() {
                let Some(given) = place(&arg.value).filter(|_| arg.is_mut) else { continue };
                let inner =
                    callee_key.as_ref().and_then(|key| self.param_changes(key, at, arg.name.as_ref()));
                for inner in inner.unwrap_or_else(|| vec![Vec::new()]) {
                    found.push((given.under(&inner), arg.value.span));
                }
            }
        }
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                found.extend(self.changes_in(&arm.body, body));
            }
        }
        for inner in crate::effects::children(expr) {
            self.calls_in(inner, body, found);
        }
    }

    /// What a method call changes under what it's called on: everything, for a built-in
    /// method that changes its value, and what the method's summary says for a `mut self` one.
    fn method_changes(&mut self, object: &Expr, method: &str, body: Option<&Body>) -> Vec<Vec<Step>> {
        let Some(ty) = body.and_then(|body| body.expr(object.span)) else { return Vec::new() };
        if let Some(found) = builtins::method(ty, method) {
            return if found.changes { vec![Vec::new()] } else { Vec::new() };
        }
        let Some(owner) = ty.type_name() else { return Vec::new() };
        let Some((key, entry)) = self.checker.program.method(owner, method) else { return Vec::new() };
        if !entry.def.params.first().is_some_and(|param| param.is_self && param.is_mut) {
            return Vec::new();
        }
        self.summary(&key).get("self").cloned().unwrap_or_default()
    }

    /// The key of the function a call names, when it's one of the program's.
    fn function_key(&self, callee: &Expr, body: Option<&Body>) -> Option<String> {
        match &callee.kind {
            ExprKind::Name(name) if self.checker.program.functions.contains_key(name) => Some(name.clone()),
            ExprKind::Attr { object, name, .. } => {
                let owner = body.and_then(|body| body.expr(object.span))?.type_name()?;
                self.checker.program.method(owner, name).map(|(key, _)| key)
            }
            _ => None,
        }
    }

    /// What a function changes under the parameter an argument is given for, by position or
    /// name. A method's `self` comes first, so positions skip it.
    fn param_changes(&mut self, key: &str, at: usize, named: Option<&String>) -> Option<Vec<Vec<Step>>> {
        let def = &self.checker.program.functions.get(key)?.def;
        let skip = usize::from(def.takes_self());
        let param = match named {
            Some(name) => def.params.iter().find(|param| &param.name == name)?,
            None => def.params.get(at + skip)?,
        };
        let name = param.name.clone();
        Some(self.summary(key).get(&name).cloned().unwrap_or_default())
    }

    /// What a function changes under each `mut` parameter, worked out once. Functions that
    /// call each other are worked out together, as one group: each pass over the group sees what
    /// the last found, until a pass finds nothing more, and then the whole group is settled.
    fn summary(&mut self, key: &str) -> Summary {
        if let Some(settled) = self.settled.0.get(key) {
            return settled.clone();
        }
        if let Some(&place) = self.places.get(key) {
            self.lowest = self.lowest.min(place);
            self.calls_back += 1;
            return self.summaries.get(key).cloned().unwrap_or_default();
        }
        if !self.checker.program.functions.contains_key(key) {
            return Summary::new();
        }
        let place = self.open.len();
        self.open.push(key.to_owned());
        self.places.insert(key.to_owned(), place);
        let outer = std::mem::replace(&mut self.lowest, place);
        let calls_back = self.calls_back;
        let found = self.worked_out(key);
        self.summaries.insert(key.to_owned(), found);
        let lowest = std::mem::replace(&mut self.lowest, outer);
        self.lowest = self.lowest.min(lowest);
        if lowest >= place {
            // One function alone needs another pass only when something called back into it.
            let again = self.open.len() > place + 1 || self.calls_back > calls_back;
            self.settle_group(place, again);
        }
        self.settled.0.get(key).or_else(|| self.summaries.get(key)).cloned().unwrap_or_default()
    }

    /// Works the group starting at `first` in `open` out again until nothing more is found,
    /// then settles it.
    fn settle_group(&mut self, first: usize, again: bool) {
        let group: Vec<String> = self.open.split_off(first);
        let mut grew = again;
        while grew {
            grew = false;
            for key in &group {
                let outer = std::mem::replace(&mut self.lowest, usize::MAX);
                let found = self.worked_out(key);
                self.lowest = outer;
                if self.summaries.get(key) != Some(&found) {
                    self.summaries.insert(key.clone(), found);
                    grew = true;
                }
            }
        }
        for key in group {
            self.places.remove(&key);
            let found = self.summaries.remove(&key).unwrap_or_default();
            self.settled.0.insert(key, found);
        }
    }

    /// One pass over a function's body: what it changes under each `mut` parameter, given what's
    /// known so far of what it calls.
    fn worked_out(&mut self, key: &str) -> Summary {
        let checker = self.checker;
        let Some(entry) = checker.program.functions.get(key) else { return Summary::new() };
        let def = &entry.def;
        let mutable: Vec<&str> =
            def.params.iter().filter(|param| param.is_mut).map(|param| param.name.as_str()).collect();
        let mut summary = Summary::new();
        for (changed, _) in self.changes_in(&def.body, checker.typed.get(key)) {
            if mutable.contains(&changed.root.as_str()) {
                let found = summary.entry(changed.root).or_default();
                if !found.contains(&changed.steps) {
                    found.push(changed.steps);
                }
            }
        }
        summary
    }
}

/// Reports each `mut` argument whose place another argument of the same call can reach, in a
/// function's body or a nested function's. What a method is called on counts as an argument.
pub(crate) fn check_arguments(stmts: &[Stmt]) -> Vec<Diagnostic> {
    let mut problems = Vec::new();
    arguments_in_block(stmts, &mut problems);
    problems
}

fn arguments_in_block(stmts: &[Stmt], problems: &mut Vec<Diagnostic>) {
    for stmt in stmts {
        for expr in exprs(stmt) {
            arguments_in(expr, problems);
        }
        for block in blocks(stmt) {
            arguments_in_block(block, problems);
        }
        if let StmtKind::Def(def) = &stmt.kind {
            arguments_in_block(&def.body, problems);
        }
    }
}

fn arguments_in(expr: &Expr, problems: &mut Vec<Diagnostic>) {
    if let ExprKind::Call { callee, args } = &expr.kind {
        overlapping_arguments(callee, args, problems);
    }
    if let ExprKind::Match { arms, .. } = &expr.kind {
        for arm in arms {
            arguments_in_block(&arm.body, problems);
        }
    }
    for inner in crate::effects::children(expr) {
        arguments_in(inner, problems);
    }
}

fn overlapping_arguments(callee: &Expr, args: &[Arg], problems: &mut Vec<Diagnostic>) {
    let receiver = match &callee.kind {
        ExprKind::Attr { object, .. } => place(object).map(|found| (found, object.as_ref())),
        _ => None,
    };
    let mut earlier: Vec<(Place, &Expr, bool)> = Vec::new();
    for arg in args {
        let Some(given) = place(&arg.value) else { continue };
        // Two `ref`s into one collection overlap however they're written, `xs[0]` and `xs[2]` too:
        // each has its place to itself. A value read beside a `ref` is apart when its index is
        // written as a different one.
        let clash = earlier.iter().find(|(other, written, is_mut)| {
            (arg.is_mut || *is_mut)
                && given.overlaps(other)
                && ((arg.is_mut && *is_mut) || !written_apart(&arg.value, written))
        });
        let held = receiver.as_ref().filter(|(called_on, _)| arg.is_mut && given.overlaps(called_on));
        if let Some((_, other, is_mut)) = clash {
            let (first, second) = (written(other, *is_mut), written(&arg.value, arg.is_mut));
            let message = format!("{first} and {second} overlap");
            let items = arg.is_mut && *is_mut && first != second && given.steps.contains(&None);
            let fix = if items { ITEMS_FIX } else { OVERLAP_FIX };
            problems.push(Diagnostic::new(message, arg.value.span).with_help(fix));
        } else if let Some((_, called_on)) = held {
            let (first, second) = (written(&arg.value, true), written(called_on, false));
            let message = format!("{first} overlaps {second}, which this method is called on");
            problems.push(Diagnostic::new(message, arg.value.span).with_help(OVERLAP_FIX));
        }
        earlier.push((given, &arg.value, arg.is_mut));
    }
    picked_by_a_mut_argument(callee, args, problems);
}

/// Every place an expression gives a call as `ref`, which the call may give a new value, with the
/// call's span.
fn given_as_ref<'e>(expr: &'e Expr, found: &mut Vec<(Place, &'e Expr, Span)>) {
    if let ExprKind::Call { args, .. } = &expr.kind {
        for arg in args.iter().filter(|arg| arg.is_mut) {
            if let Some(given) = place(&arg.value) {
                found.push((given, &arg.value, expr.span));
            }
        }
    }
    for inner in crate::effects::children(expr) {
        given_as_ref(inner, found);
    }
}

/// Whether two places are written with different indexes that are known as written, like `xs[0]`
/// and `xs[1]`, so they can't be one item. `xs[-1]` could be `xs[0]`, so an index counted from the
/// end isn't known.
fn written_apart(first: &Expr, second: &Expr) -> bool {
    steps_of(first).iter().zip(&steps_of(second)).any(|(one, other)| match (&one.kind, &other.kind) {
        (ExprKind::Int(one), ExprKind::Int(other)) => one != other,
        (ExprKind::Str(one), ExprKind::Str(other)) => one != other,
        _ => false,
    })
}

/// What each step along a place goes by, from its root: an index, or for a field the field's own
/// expression, which is never told apart here, since `Place::overlaps` compares fields.
fn steps_of(expr: &Expr) -> Vec<&Expr> {
    let (object, step) = match &expr.kind {
        ExprKind::Attr { object, .. } => (object, expr),
        ExprKind::Index { object, index } => (object, index.as_ref()),
        _ => return Vec::new(),
    };
    let mut steps = steps_of(object);
    steps.push(step);
    steps
}

/// `units[at].step(mut at)` and `f(mut xs[i], mut i)`: a `mut` argument changes what picks the
/// place another argument names, so which place that is would depend on when it's read.
fn picked_by_a_mut_argument(callee: &Expr, args: &[Arg], problems: &mut Vec<Diagnostic>) {
    let receiver = match &callee.kind {
        ExprKind::Attr { object, .. } => Some(object.as_ref()),
        _ => None,
    };
    let others = || receiver.into_iter().chain(args.iter().map(|arg| &arg.value));
    for arg in args.iter().filter(|arg| arg.is_mut) {
        let Some(changed) = place(&arg.value) else { continue };
        let picked = others().find(|other| {
            other.span != arg.value.span && place(other).is_some() && picks_with(other, &changed)
        });
        if let Some(other) = picked {
            let (first, second) = (written(&arg.value, true), written(other, false));
            let message = format!("{first} changes what picks {second}");
            problems.push(
                Diagnostic::new(message, arg.value.span)
                    .with_help("hold the index in another name, and give that name as `ref`"),
            );
        }
    }
}

/// Whether an index along a place reads `changed`.
fn picks_with(expr: &Expr, changed: &Place) -> bool {
    match &expr.kind {
        ExprKind::Attr { object, .. } => picks_with(object, changed),
        ExprKind::Index { object, index } => reads(index, changed) || picks_with(object, changed),
        _ => false,
    }
}

fn reads(expr: &Expr, changed: &Place) -> bool {
    place(expr).is_some_and(|found| found.overlaps(changed))
        || crate::effects::children(expr).into_iter().any(|inner| reads(inner, changed))
}

/// The rule behind each of these errors, said first in each one's help so they read as one rule:
/// `f(ref xs[i], ref xs[j])`, `bag.grab(bag.items)` and an argument that changes what its call
/// changes all reach one thing two ways while it changes.
macro_rules! one_way_in {
    ($fix:literal) => {
        concat!("a call has to be the only way to reach what it changes: ", $fix)
    };
}

const ARGUMENT_CHANGES_FIX: &str = one_way_in!(
    "make that change on the line before, keeping what it gives back in a name, and pass the name"
);

const OVERLAP_FIX: &str =
    one_way_in!("pass a value you made first, like `item = xs[j]`, or make the changes one at a time");

const RECEIVER_FIX: &str = one_way_in!("pass a value you made first, like `held = bag.items`");

/// Two `ref`s into one collection, at indexes written apart or not.
const ITEMS_FIX: &str = one_way_in!(
    "two items of one collection may be the same one, even at different indexes, so make the changes one at a time"
);

/// Whether a value is passed as itself, leaving nothing behind that the callee could change.
fn passed_as_a_value(ty: Option<&Type>) -> bool {
    matches!(ty, Some(Type::Bool | Type::Int | Type::Float | Type::Sized(_) | Type::None | Type::Unknown))
}

/// An argument that names a place, in backticks as it's written.
fn written(expr: &Expr, is_mut: bool) -> String {
    let word = if is_mut { "ref " } else { "" };
    format!("`{word}{}`", place_text(expr))
}

pub(crate) fn place_text(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Name(name) => name.clone(),
        ExprKind::Attr { object, name, .. } => format!("{}.{name}", place_text(object)),
        ExprKind::Index { object, index } => {
            let at = match &index.kind {
                ExprKind::Name(name) => name.clone(),
                ExprKind::Int(value) => value.to_string(),
                _ => "...".to_owned(),
            };
            format!("{}[{at}]", place_text(object))
        }
        _ => "...".to_owned(),
    }
}

/// The blocks directly inside a statement, but not a nested function's.
pub(crate) fn blocks(stmt: &Stmt) -> Vec<&[Stmt]> {
    match &stmt.kind {
        StmtKind::If { branches, otherwise } => {
            branches.iter().map(|(_, block)| block.as_slice()).chain(otherwise.as_deref()).collect()
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => vec![body],
        _ => Vec::new(),
    }
}

/// The expressions written directly in a statement.
pub(crate) fn exprs(stmt: &Stmt) -> Vec<&Expr> {
    match &stmt.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Lend { value, .. }
        | StmtKind::Return(Some(value)) => vec![value],
        StmtKind::Assign { target, value, .. } => vec![target, value],
        StmtKind::If { branches, .. } => branches.iter().map(|(cond, _)| cond).collect(),
        StmtKind::While { cond: value, .. }
        | StmtKind::For { iter: value, .. }
        | StmtKind::With { value, .. } => {
            vec![value]
        }
        StmtKind::OnFailure { .. }
        | StmtKind::Return(None)
        | StmtKind::Def(_)
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Pass => Vec::new(),
    }
}

/// Whether a statement uses a name anywhere in it.
fn mentions(stmt: &Stmt, name: &str) -> bool {
    let mut found = false;
    fennec_syntax::read::in_stmts(std::slice::from_ref(stmt), &mut |expr| {
        found |= matches!(&expr.kind, ExprKind::Name(named) if named == name);
    });
    found
}

/// Each `ref name = place` in some statements, with the span of its place.
fn refs_in(stmts: &[Stmt], found: &mut Vec<(String, Span)>) {
    for stmt in stmts {
        if let Some((name, held)) = stmt.kind.local_ref() {
            found.push((name.to_owned(), held.span));
        }
        for block in blocks(stmt) {
            refs_in(block, found);
        }
    }
}
