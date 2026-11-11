//! Writes each `with` over a source in as the source's own body, once a program checks, so
//! every engine runs it with nothing new. `with transaction()? as tx:` and its block become the
//! source's statements, its parameters bound to the arguments and its names hidden, with the
//! `lend` replaced by `tx = ...` and the caller's block:
//!
//! - The block's `return`, `break` and `continue` leave a loop that runs it once, and are taken
//!   again after the source's cleanup, so cleanup below the `lend` always runs on success.
//! - Each `fail` and `?` in the block runs the `lend`'s `else:` first, so it runs whenever the
//!   caller's block fails. A `?` is taken out to its own `match` on the result before its
//!   statement.
//!
//! It's the only place that knows how a source runs, so a native one can replace it alone.

use std::collections::BTreeMap;
use std::sync::Arc;

use fennec_syntax::{
    Expr, ExprKind, FnDef, Item, MatchArm, Module, Pattern, Span, Stmt, StmtKind, TypeExpr, WithName,
};

/// A source a `with` uses: its definition, and whether it's a method, called on a value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    pub(crate) def: Arc<FnDef>,
    pub(crate) method: bool,
    /// What it gives where it's called, `with T` as that call has it: the types its type
    /// parameters stand for there, as in `with Channel[Job]`.
    pub(crate) gives: crate::types::Type,
    /// The names of its type parameters, a method's type's first.
    pub(crate) generics: Vec<String>,
}

/// Writes in every `with` over a source in `module`, saying whether there were any.
pub(crate) fn lower(
    module: &mut Module,
    sources: &BTreeMap<(u32, u32), Source>,
    launches: &BTreeMap<(u32, u32), crate::launch::Launch>,
    ref_matches: &BTreeMap<(u32, u32), Vec<Option<Vec<String>>>>,
) -> bool {
    if sources.is_empty() && launches.is_empty() && ref_matches.is_empty() {
        return false;
    }
    // What an earlier pass wrote in keeps its names and spans: a source written in now, inside one
    // written in then, gets ones after them, never the same.
    let (mut lowest, mut written_low, mut made) = (u32::MAX, Span::PRELUDE, 0);
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        if expr.span.start >= COPIES_FROM {
            lowest = lowest.min(expr.span.start);
        }
        if (WRITTEN_FROM..Span::PRELUDE).contains(&expr.span.start) {
            written_low = written_low.min(expr.span.start);
        }
        if let ExprKind::Name(name) = &expr.kind {
            made = made.max(made_in(name));
        }
    });
    NEXT.with(|next| next.set(written_low - 1));
    COPIES.with(|copies| copies.set(lowest));
    let mut lowering = Lowering {
        sources,
        launches,
        ref_matches,
        made,
        returns: Vec::new(),
        failing: Vec::new(),
        instead: None,
    };
    for item in &mut module.items {
        match item {
            Item::Function(def) => lowering.function(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                def.methods.iter_mut().for_each(|def| lowering.function(def));
            }
            Item::Extend(extension) => extension.methods.iter_mut().for_each(|def| lowering.function(def)),
            Item::Test(test) => {
                lowering.returns.push(None);
                lowering.stmts(&mut test.body);
                lowering.returns.pop();
            }
            _ => {}
        }
    }
    // Only what this pass wrote in is a change: `made` starts from what earlier passes made.
    let wrote = lowering.made > made;
    if wrote {
        discards_apart_again(module);
    }
    wrote
}

/// Writes each `with` that opens several handles, `with a()? as x, b()? as y:`, as one inside
/// another, saying whether there were any. Then they finish innermost first, and when one fails
/// to open, the ones opened before it run their `else:`, as their blocks failed.
pub(crate) fn nest(module: &mut Module, opened_apart: &std::collections::BTreeSet<(u32, u32)>) -> bool {
    if opened_apart.is_empty() {
        return false;
    }
    let mut nested = false;
    fennec_syntax::visit::functions_mut(module, &mut |def| nest_in(&mut def.body, opened_apart, &mut nested));
    for item in &mut module.items {
        if let Item::Test(test) = item {
            nest_in(&mut test.body, opened_apart, &mut nested);
        }
    }
    // A `match` case holds statements too. Nesting one twice finds nothing the second time.
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        if let ExprKind::Match { arms, .. } = &mut expr.kind {
            for arm in arms {
                nest_in(&mut arm.body, opened_apart, &mut nested);
            }
        }
    });
    nested
}

fn nest_in(body: &mut [Stmt], opened_apart: &std::collections::BTreeSet<(u32, u32)>, nested: &mut bool) {
    for each in body {
        match &mut each.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    nest_in(body, opened_apart, nested);
                }
                if let Some(body) = otherwise {
                    nest_in(body, opened_apart, nested);
                }
            }
            StmtKind::Lend { otherwise, .. } => nest_in(otherwise, opened_apart, nested),
            StmtKind::Def(def) => nest_in(&mut def.body, opened_apart, nested),
            StmtKind::With { value, names, body, lock: false } => {
                if opened_apart.contains(&(value.span.start, value.span.end))
                    && let ExprKind::Tuple(items) = &value.kind
                    && items.len() == names.len()
                    && let Some(first) = items.first().cloned()
                {
                    let mut inner = std::mem::take(body);
                    for (item, held) in items.iter().zip(names.iter()).skip(1).rev() {
                        let names = vec![held.clone()];
                        let kind = StmtKind::With { value: item.clone(), names, body: inner, lock: false };
                        inner = vec![Stmt { kind, span: each.span }];
                    }
                    names.truncate(1);
                    *value = first;
                    *body = inner;
                    *nested = true;
                }
                nest_in(body, opened_apart, nested);
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => {
                nest_in(body, opened_apart, nested);
            }
            _ => {}
        }
    }
}

/// Gives each `_` its own name again: a source's `else:` is written in at each place the block
/// can fail, and each copy repeats its `_ = ...`. No one can write the name, so nothing reads it.
fn discards_apart_again(module: &mut Module) {
    let discard = format!("_{}", fennec_syntax::visit::HIDDEN);
    let mut made = 0;
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind
            && name.starts_with(&discard)
        {
            expr.kind = ExprKind::Name(format!("{discard}{made}"));
            made += 1;
        }
    });
}

struct Lowering<'s> {
    sources: &'s BTreeMap<(u32, u32), Source>,
    /// Each `with launch(...)`, which is written in as a source made for it.
    launches: &'s BTreeMap<(u32, u32), crate::launch::Launch>,
    /// Each `match` with a `ref` case, with the fields each such case puts back.
    ref_matches: &'s BTreeMap<(u32, u32), Vec<Option<Vec<String>>>>,
    /// How many have been written in, which makes each one's hidden names its own.
    made: usize,
    /// The return type written for each function being walked, innermost last.
    returns: Vec<Option<TypeExpr>>,
    /// Whether each function being walked says it fails, innermost last.
    failing: Vec<bool>,
    /// What the source being written in does in place of failing, for `with src else fail E:`.
    instead: Option<Expr>,
}

thread_local! {
    /// The next span for something a lowering writes, counting down from just below the
    /// standard library's, where no program's text reaches.
    static NEXT: std::cell::Cell<u32> = const { std::cell::Cell::new(Span::PRELUDE - 1) };
    /// Where the next copy of a generic source's text goes, counting down from the top: still
    /// the standard library's, so it keeps its own privacy, but past where its text reaches.
    static COPIES: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
}

/// Where copies of std text go: far past the end of the standard library's own text.
const COPIES_FROM: u32 = Span::PRELUDE + (1 << 30);

/// Moves the standard library's text in a written-in copy of a generic source to spans of its own,
/// each where it was against the rest, below every copy made so far.
fn moved_apart(written: &mut [Stmt], body: &mut [Stmt]) {
    let (mut low, mut high) = (u32::MAX, 0);
    for part in [&mut *written, &mut *body] {
        fennec_syntax::visit::in_stmts(part, &mut |expr| {
            if expr.span.in_prelude() && expr.span.start < COPIES_FROM {
                low = low.min(expr.span.start);
                high = high.max(expr.span.end);
            }
        });
    }
    if low > high {
        return;
    }
    let base = COPIES.with(|copies| {
        let base = copies.get().saturating_sub(high - low + 1);
        copies.set(base);
        base
    });
    for part in [written, body] {
        fennec_syntax::visit::in_stmts(part, &mut |expr| {
            if expr.span.in_prelude() && expr.span.start < COPIES_FROM {
                expr.span = Span { start: expr.span.start - low + base, end: expr.span.end - low + base };
            }
        });
    }
}

/// Starts the spans lowering writes below every one written in so far in `module`, so none is
/// another's.
pub(crate) fn below_written(module: &mut Module) {
    let mut written_low = Span::PRELUDE;
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        if written_in(expr.span) {
            written_low = written_low.min(expr.span.start);
        }
    });
    NEXT.with(|next| next.set(next.get().min(written_low - 1)));
}

/// Reserves `length` spans in a row for text written in, giving the lowest.
pub(crate) fn reserved(length: u32) -> u32 {
    NEXT.with(|next| {
        let low = next.get() - length;
        next.set(low - 1);
        low
    })
}

/// The lowest a span a lowering writes can be: well above any program's text, and below the
/// standard library's.
const WRITTEN_FROM: u32 = Span::PRELUDE - (1 << 28);

/// Whether lowering wrote what's at `span` in, rather than a program or the standard library.
pub(crate) fn written_in(span: Span) -> bool {
    (WRITTEN_FROM..Span::PRELUDE).contains(&span.start)
}

/// The number a name written in by an earlier pass ends with, like `3` in `conn__lend3`, or 0.
fn made_in(name: &str) -> usize {
    ["__lend", "__ref"]
        .iter()
        .filter_map(|mark| name.rsplit_once(mark))
        .filter_map(|(_, number)| number.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
}

/// Where a lowering writes something of its own: a span covering nothing, its own, so the
/// types the program is checked again with, found by span, are never another's.
pub(crate) fn here(_near: Span) -> Span {
    let at = NEXT.with(|next| {
        let at = next.get();
        next.set(at - 1);
        at
    });
    Span { start: at, end: at }
}

pub(crate) fn name(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Name(name.to_owned()), span: here(span) }
}

pub(crate) fn assign(target: &str, value: Expr, declare_mut: bool, ty: Option<TypeExpr>) -> Stmt {
    let span = value.span;
    let kind = StmtKind::Assign { target: name(target, span), op: None, value, declare_mut, ty };
    Stmt { kind, span: here(span) }
}

pub(crate) fn int(value: i64, span: Span) -> Expr {
    Expr { kind: ExprKind::Int(value), span: here(span) }
}

/// `exit == kind`, for taking a block's way out again.
fn exits_by(exit: &str, kind: i64, span: Span) -> Expr {
    let lhs = Box::new(name(exit, span));
    let rhs = Box::new(int(kind, span));
    Expr { kind: ExprKind::Binary { op: fennec_syntax::BinOp::Eq, lhs, rhs }, span: here(span) }
}

pub(crate) fn stmt(kind: StmtKind, span: Span) -> Stmt {
    Stmt { kind, span: here(span) }
}

/// The ways out of the caller's block, by the number its `exit` is set to.
const RETURNED: i64 = 1;
const BROKE: i64 = 2;
const CONTINUED: i64 = 3;

impl Lowering<'_> {
    fn function(&mut self, def: &mut FnDef) {
        self.returns.push(def.ret.clone());
        self.failing.push(def.fails.is_some());
        if def.ret.is_some() {
            self.tail_returned(&mut def.body);
        }
        self.stmts(&mut def.body);
        self.failing.pop();
        self.returns.pop();
    }

    /// A function's last line that's a `with` over a source, whose block's last line is a value:
    /// that value is what the function gives back. The source's code after its `lend` is written
    /// in after the block, so the value is written as `return value`, which leaves after it.
    fn tail_returned(&self, body: &mut [Stmt]) {
        let Some(last) = body.last_mut() else { return };
        match &mut last.kind {
            StmtKind::With { value, body: block, lock: false, .. } if self.is_source(value) => {
                let Some(inner) = block.last_mut() else { return };
                match &mut inner.kind {
                    StmtKind::Expr(value) => inner.kind = StmtKind::Return(Some(value.clone())),
                    _ => self.tail_returned(block),
                }
            }
            StmtKind::If { branches, otherwise: Some(otherwise) } => {
                for (_, branch) in branches {
                    self.tail_returned(branch);
                }
                self.tail_returned(otherwise);
            }
            StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
                for arm in arms {
                    self.tail_returned(&mut arm.body);
                }
            }
            _ => {}
        }
    }

    /// Whether what a `with` takes is a call to a source, through a `?` or an `else` that leaves.
    fn is_source(&self, value: &Expr) -> bool {
        let call = match &value.kind {
            ExprKind::Try { value: inner, context: None } => inner.as_ref(),
            ExprKind::Else { value: inner, default } if default.leaves() => inner.as_ref(),
            _ => value,
        };
        self.sources.contains_key(&(call.span.start, call.span.end))
    }

    /// `block`, with `cleanup` also run for what fails inside what it calls, which can only
    /// pass up from a function that says it fails.
    fn on_failure(&self, cleanup: &[Stmt], block: Vec<Stmt>, at: Span) -> Vec<Stmt> {
        if cleanup.is_empty() || self.failing.last() != Some(&true) {
            return block;
        }
        let held = format!("failed__within{}", here(at).start);
        let cleanup = failing_with(name(&held, at), cleanup, at);
        vec![stmt(StmtKind::OnFailure { name: held, cleanup, body: block }, at)]
    }

    fn stmts(&mut self, body: &mut Vec<Stmt>) {
        let mut at = 0;
        while at < body.len() {
            let Some(stmt) = body.get_mut(at) else { return };
            self.inner(stmt);
            let written = match &stmt.kind {
                StmtKind::With { value, names, body: block, lock: false } => {
                    // `with src else fail E as x:`: every way the source fails does that instead.
                    self.instead = None;
                    let call = match &value.kind {
                        ExprKind::Try { value: inner, context: None } => inner.as_ref(),
                        ExprKind::Else { value: inner, default } if default.leaves() => {
                            self.instead = Some(default.as_ref().clone());
                            inner.as_ref()
                        }
                        _ => value,
                    };
                    let at = (call.span.start, call.span.end);
                    // A source whose block still holds a `with` waits for that one to be written
                    // in, as a source just written in holds ones that aren't known till it's
                    // checked again: so the failures of what's written there run this one's
                    // `else:` too, and their `?`s stay theirs.
                    if holds_with(block) {
                        None
                    } else {
                        match (self.sources.get(&at).cloned(), self.launches.get(&at)) {
                            (Some(source), _) => self.expand(call, names, block, &source, stmt.span),
                            (None, Some(launch)) => crate::launch::source(launch).and_then(|source| {
                                let options = crate::launch::options_only(call);
                                self.expand(&options, names, block, &source, stmt.span)
                            }),
                            (None, None) => None,
                        }
                    }
                }
                StmtKind::Expr(value)
                    if self.ref_matches.contains_key(&(value.span.start, value.span.end)) =>
                {
                    self.ref_match(value, stmt.span)
                }
                _ => None,
            };
            match written {
                Some(written) => {
                    let count = written.len();
                    body.splice(at..=at, written);
                    at += count;
                }
                None => at += 1,
            }
        }
    }

    /// The blocks inside a statement, written in first, so a `with` inside another is.
    fn inner(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    self.stmts(body);
                }
                if let Some(body) = otherwise {
                    self.stmts(body);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => {
                self.stmts(body);
            }
            StmtKind::Def(def) => self.function(def),
            StmtKind::Expr(value) | StmtKind::Assign { value, .. } => {
                if let ExprKind::Match { arms, .. } = &mut value.kind {
                    for arm in arms {
                        self.stmts(&mut arm.body);
                    }
                }
            }
            _ => {}
        }
    }

    /// A `match` with `ref` cases, each written as a case that binds the field as a plain name
    /// and puts the case back, with that name's value, wherever its body ends: when it finishes,
    /// leaves by `return`, `break` or `continue`, or fails.
    fn ref_match(&mut self, value: &Expr, at: Span) -> Option<Vec<Stmt>> {
        let ExprKind::Match { subject, arms } = &value.kind else { return None };
        let cases = self.ref_matches.get(&(value.span.start, value.span.end))?;
        let mut written = arms.clone();
        for (arm, fields) in written.iter_mut().zip(cases) {
            let Some(fields) = fields else { continue };
            self.ref_case(arm, subject, fields, at)?;
        }
        let kind = ExprKind::Match { subject: subject.clone(), arms: written };
        Some(vec![stmt(StmtKind::Expr(Expr { kind, span: value.span }), at)])
    }

    /// One `ref` case, written in: its `ref`s and `_`s bound to hidden names, the `ref`s copied
    /// to mut names of their own for the body, and the case put back, made from them, on every
    /// way out.
    fn ref_case(&mut self, arm: &mut MatchArm, subject: &Expr, fields: &[String], at: Span) -> Option<()> {
        let Pattern::Variant { name: case, args, .. } = &mut arm.pattern else { return None };
        if args.len() != fields.len() {
            return None;
        }
        self.made += 1;
        let made = self.made;
        let mut copies = Vec::new();
        let mut parts = Vec::new();
        for (at_field, (arg, field)) in args.iter_mut().zip(fields).enumerate() {
            let value = match arg {
                Pattern::RefBind(held) => {
                    let bound = format!("{held}__ref{made}");
                    copies.push(assign(held, name(&bound, at), true, None));
                    let value = name(held, at);
                    *arg = Pattern::Bind(bound);
                    value
                }
                Pattern::Bind(held) => name(held, at),
                Pattern::Wildcard => {
                    let bound = format!("field{at_field}__ref{made}");
                    *arg = Pattern::Bind(bound.clone());
                    name(&bound, at)
                }
                Pattern::Literal(literal) => literal.clone(),
                _ => return None,
            };
            parts.push(fennec_syntax::Arg { name: Some(field.clone()), is_mut: false, value });
        }
        let variant = Expr { kind: ExprKind::Variant(case.clone()), span: here(at) };
        let rebuilt =
            Expr { kind: ExprKind::Call { callee: Box::new(variant), args: parts }, span: here(at) };
        let put_back = stmt(
            StmtKind::Assign {
                target: subject.clone(),
                op: None,
                value: rebuilt,
                declare_mut: false,
                ty: None,
            },
            at,
        );
        let mut block = std::mem::take(&mut arm.body);
        fail_through(&mut block, std::slice::from_ref(&put_back));
        let (exit, returned) = (format!("exit__ref{made}"), format!("returned__ref{made}"));
        let mut caller = Caller { exit: &exit, returned: &returned, ways: Ways::default(), span: at };
        exits(&mut block, &mut caller, false);
        let ways = caller.ways;
        let mut block = self.on_failure(std::slice::from_ref(&put_back), block, at);
        let mut body = copies;
        if ways.any() {
            body.push(assign(&exit, int(0, at), true, None));
            if ways.returned == Some(true) {
                let mut optional = self.returns.last().cloned().flatten()?;
                optional.optional = true;
                let none = Expr { kind: ExprKind::None, span: here(at) };
                body.push(assign(&returned, none, true, Some(optional)));
            }
            block.push(stmt(StmtKind::Break, at));
            let always = Expr { kind: ExprKind::Bool(true), span: here(at) };
            body.push(stmt(StmtKind::While { cond: always, body: block }, at));
        } else {
            body.extend(block);
        }
        body.push(put_back);
        body.extend(taken_again(ways, &exit, &returned, at));
        arm.body = body;
        Some(())
    }

    /// `with call as name: block` written in as the source's body, or nothing if it can't be.
    fn expand(
        &mut self,
        call: &Expr,
        names: &[WithName],
        block: &[Stmt],
        source: &Source,
        at: Span,
    ) -> Option<Vec<Stmt>> {
        let ExprKind::Call { callee, args } = &call.kind else { return None };
        // A block that always returns leaves the source's body returning, which what's written
        // after it says, so a function ending in the `with` gives back a value on every way.
        let always_returns = always_returns(block);
        // Several names split what the source lends, a tuple, as `with conn.split() as r, w:`.
        let split = names.len() > 1;
        let held = match names {
            [held] => held.clone(),
            [first, ..] => WithName { name: format!("{}__split", first.name), ..first.clone() },
            // A `with` with no name still runs what's lent, as `with launch(() => work())?:` does.
            [] => WithName { name: "unnamed__held".to_owned(), is_mut: false, span: at },
        };
        self.made += 1;
        let suffix = format!("__lend{}", self.made);
        let hidden = |name: &str| format!("{name}{suffix}");
        let def = source.def.as_ref();
        let (mut written, mut renamed) = bound(def, callee, args, source.method, &hidden)?;
        // The source's own names, hidden, so none is one of the caller's.
        let mut locals = Vec::new();
        declared_in(&def.body, &mut locals);
        renamed.extend(locals.iter().map(|local| (local.clone(), hidden(local))));
        let mut body = def.body.clone();
        rename(&mut body, &renamed);
        if let Some(instead) = self.instead.take() {
            fail_instead(&mut body, &instead);
        }
        typed_for_call(source, &mut written, &mut body)?;
        // Types are found by span, and each call gives the body types of its own, as `None` for a
        // parameter one call leaves out and another gives, so each written-in copy of a source's
        // text gets spans of its own.
        moved_apart(&mut written, &mut body);
        // The caller's name for what's lent: the source's own name for it, when it lends one,
        // so the block's changes are what the source's cleanup sees, or else a name of its own,
        // apart from the source's hidden ones.
        let lent_as = format!("{}__lent{}", held.name, self.made);
        let exit = hidden("exit");
        let returned = hidden("returned");
        let caller_ret = self.returns.last().cloned().flatten();
        let mut caller = Caller { exit: &exit, returned: &returned, ways: Ways::default(), span: at };
        if !replace_lend(&mut body, &mut |value, otherwise| {
            let mut block = block.to_vec();
            let (place, binding) = match &value.kind {
                ExprKind::Name(place) => (place.clone(), None),
                _ => (lent_as.clone(), Some(assign(&lent_as, value, true, None))),
            };
            given_as(&mut block, (split, names), &held.name, &place, at);
            // A generator let go at a `yield` in the block goes on once, to leave by `return`,
            // so the source's cleanup runs as it does for any other way out.
            leave_when_abandoned(&mut block, at);
            // A crash runs the `else:` too: it's a function of its own, marked where the block
            // starts and unmarked wherever the block ends or fails, before any cleanup runs.
            let crash = on_crash(otherwise, at);
            let unmarked = unmarked_first(crash.as_ref(), otherwise);
            if !otherwise.is_empty() {
                fail_through(&mut block, &unmarked);
            }
            caller.ways = Ways::default();
            exits(&mut block, &mut caller, false);
            let mut block = self.on_failure(&unmarked, block, at);
            let mut lent: Vec<Stmt> = binding.into_iter().collect();
            if let Some((made, marked, _)) = &crash {
                lent.push(made.clone());
                lent.push(marked.clone());
            }
            if caller.ways.any() {
                block.push(stmt(StmtKind::Break, at));
                let always = Expr { kind: ExprKind::Bool(true), span: here(at) };
                lent.push(stmt(StmtKind::While { cond: always, body: block }, at));
            } else {
                lent.extend(block);
            }
            if let Some((_, _, done)) = crash {
                lent.push(done);
            }
            Some(lent)
        }) {
            return None;
        }
        // The block's way out, kept in names set before the source's body, so they're in sight
        // after it, and taken again once its cleanup has run.
        let ways = caller.ways;
        if ways.any() {
            written.push(assign(&exit, int(0, at), true, None));
        }
        if ways.returned == Some(true) {
            let mut optional = caller_ret?;
            optional.optional = true;
            let none = Expr { kind: ExprKind::None, span: here(at) };
            written.push(assign(&returned, none, true, Some(optional)));
        }
        written.extend(body);
        if always_returns && ways.returned == Some(true) && !ways.broke && !ways.continued {
            written.extend(returned_always(&returned, at));
        } else {
            written.extend(taken_again(ways, &exit, &returned, at));
        }
        Some(written)
    }
}

/// What each of a generic source's type parameters stands for at a call, from what the call
/// gives, `with Channel[Job]`, against what the source says it gives, `with Channel[T]`: each as
/// a written type and as it's written in brackets, `Channel[T](...)`. `None` when one can't be
/// written.
fn given_types(
    def: &FnDef,
    generics: &[String],
    lent: &crate::types::Type,
) -> Option<BTreeMap<String, (TypeExpr, Expr)>> {
    let names: Vec<&str> = generics.iter().map(String::as_str).collect();
    let mut found = BTreeMap::new();
    if let Some(ret) = &def.ret {
        match_params(ret, lent, &names, &mut found);
    }
    let mut given = BTreeMap::new();
    for (name, ty) in found {
        given.insert(name, written_type(&ty)?);
    }
    Some(given)
}

/// Finds each of `names` in a written type, beside what the same place holds in `ty`.
fn match_params(
    written: &TypeExpr,
    ty: &crate::types::Type,
    names: &[&str],
    found: &mut BTreeMap<String, crate::types::Type>,
) {
    if written.args.is_empty() && names.contains(&written.name.as_str()) {
        found.insert(written.name.clone(), ty.clone());
        return;
    }
    for (arg, held) in written.args.iter().zip(ty.type_args()) {
        match_params(arg, &held, names, found);
    }
}

/// A type as written where a type goes and as written in brackets, parsed from its source and
/// moved to spans of its own, which no program's text reaches.
pub(crate) fn written_type(ty: &crate::types::Type) -> Option<(TypeExpr, Expr)> {
    let source = ty.source()?;
    let text = format!("def held__(held: {source}):\n    _ = held[{source}]\n");
    let module = fennec_syntax::parse(&text).ok()?;
    let Some(Item::Function(def)) = module.items.first() else { return None };
    let mut written = def.params.first()?.ty.clone()?;
    let StmtKind::Assign { value, .. } = &def.body.first()?.kind else { return None };
    let ExprKind::Index { index, .. } = &value.kind else { return None };
    let mut bracketed = index.as_ref().clone();
    // Spans of their own, apart from every other's, keeping where each starts against the others.
    let length = u32::try_from(text.len()).ok()?;
    let mut low = here(Span::default()).start;
    for _ in 1..length {
        low = here(Span::default()).start;
    }
    let moved = |span: Span| Span { start: low + span.start, end: low + span.end };
    fennec_syntax::visit::in_expr(&mut bracketed, &mut |expr| expr.span = moved(expr.span));
    respan(&mut written, &moved);
    Some((written, bracketed))
}

fn respan(written: &mut TypeExpr, moved: &dyn Fn(Span) -> Span) {
    written.span = moved(written.span);
    for arg in &mut written.args {
        respan(arg, moved);
    }
}

/// Writes in what each type parameter stands for: in written types, as in `held: list[T]`, and in
/// brackets, as in `Channel[T](...)`.
pub(crate) fn retype(body: &mut [Stmt], given: &BTreeMap<String, (TypeExpr, Expr)>) {
    // What's written in can name the caller's own type parameters, like `T` in `Box[T]` for a
    // source's `T`, and the walk goes on into it: each name is marked first, then the marks are
    // written in, so nothing written in is retyped again.
    fennec_syntax::visit::in_stmts(body, &mut |expr| {
        if let ExprKind::Name(name) = &mut expr.kind
            && given.contains_key(name.as_str())
        {
            name.insert_str(0, RETYPED);
        }
    });
    fennec_syntax::visit::in_stmts(body, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind
            && let Some((_, bracketed)) = name.strip_prefix(RETYPED).and_then(|name| given.get(name))
        {
            *expr = bracketed.clone();
        }
    });
    retype_written(body, given);
}

/// What marks a name to retype: no name a program writes starts with it.
const RETYPED: &str = "\0retyped:";

fn retype_written(body: &mut [Stmt], given: &BTreeMap<String, (TypeExpr, Expr)>) {
    for each in body {
        match &mut each.kind {
            StmtKind::Assign { ty: Some(ty), .. } => retype_one(ty, given),
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    retype_written(body, given);
                }
                if let Some(body) = otherwise {
                    retype_written(body, given);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => {
                retype_written(body, given);
            }
            StmtKind::Lend { otherwise, .. } => retype_written(otherwise, given),
            _ => {}
        }
    }
}

/// A generic source's text, written in at a call: its type parameters are the call's types here.
/// One named only in a parameter's type, like `E` in `make: () -> T fails E`, isn't in what the
/// source gives, so isn't known here: a name with such a type takes its value's.
fn typed_for_call(source: &Source, written: &mut [Stmt], body: &mut [Stmt]) -> Option<()> {
    let given = given_types(source.def.as_ref(), &source.generics, &source.gives)?;
    if !given.is_empty() {
        retype(written, &given);
        retype(body, &given);
    }
    let unknown: Vec<&str> =
        source.generics.iter().map(String::as_str).filter(|name| !given.contains_key(*name)).collect();
    if !unknown.is_empty() {
        untyped_where_unknown(written, &unknown);
        untyped_where_unknown(body, &unknown);
    }
    Some(())
}

/// Drops the written type of each name whose type names one of `unknown`, so it takes its value's.
fn untyped_where_unknown(body: &mut [Stmt], unknown: &[&str]) {
    fn names(ty: &TypeExpr, unknown: &[&str]) -> bool {
        unknown.contains(&ty.name.as_str()) || ty.args.iter().any(|arg| names(arg, unknown))
    }
    for each in body {
        match &mut each.kind {
            StmtKind::Assign { ty, .. } => {
                if ty.as_ref().is_some_and(|ty| names(ty, unknown)) {
                    *ty = None;
                }
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    untyped_where_unknown(body, unknown);
                }
                if let Some(body) = otherwise {
                    untyped_where_unknown(body, unknown);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. }
            | StmtKind::Lend { otherwise: body, .. } => untyped_where_unknown(body, unknown),
            _ => {}
        }
    }
}

pub(crate) fn retype_one(written: &mut TypeExpr, given: &BTreeMap<String, (TypeExpr, Expr)>) {
    if written.args.is_empty()
        && let Some((ty, _)) = given.get(&written.name)
    {
        let optional = written.optional;
        *written = ty.clone();
        written.optional |= optional;
        return;
    }
    for arg in &mut written.args {
        retype_one(arg, given);
    }
}

/// Each parameter given its value, and the value a method is called on: a `ref` parameter is
/// the caller's own name for what it changes, and the others are copies, given their values
/// first, under hidden names.
fn bound(
    def: &FnDef,
    callee: &Expr,
    args: &[fennec_syntax::Arg],
    method: bool,
    hidden: &dyn Fn(&str) -> String,
) -> Option<Bound> {
    let receiver = match (&callee.kind, method) {
        (ExprKind::Attr { object, .. }, true) => Some(object.as_ref().clone()),
        _ => None,
    };
    let mut positional = args.iter().filter(|arg| arg.name.is_none());
    let (mut written, mut renamed) = (Vec::new(), Vec::new());
    for param in &def.params {
        let value = if param.is_self {
            receiver.clone()?
        } else if let Some(arg) = args.iter().find(|arg| arg.name.as_deref() == Some(param.name.as_str())) {
            arg.value.clone()
        } else if let Some(arg) = positional.next() {
            arg.value.clone()
        } else {
            param.default.clone()?
        };
        if param.is_mut {
            let ExprKind::Name(place) = &value.kind else { return None };
            renamed.push((param.name.clone(), place.clone()));
        } else {
            renamed.push((param.name.clone(), hidden(&param.name)));
            written.push(assign(&hidden(&param.name), value, false, param.ty.clone()));
        }
    }
    Some((written, renamed))
}

/// The caller's block's way out, taken again once the source's cleanup has run.
fn taken_again(ways: Ways, exit: &str, returned: &str, at: Span) -> Vec<Stmt> {
    let mut written = Vec::new();
    match ways.returned {
        Some(true) => {
            // `if returned is not None: return returned`, where it always is.
            let none = Box::new(Expr { kind: ExprKind::None, span: here(at) });
            let lhs = Box::new(name(returned, at));
            let given = Expr {
                kind: ExprKind::Binary { op: fennec_syntax::BinOp::IsNot, lhs, rhs: none },
                span: here(at),
            };
            let back = stmt(StmtKind::Return(Some(name(returned, at))), at);
            let then = stmt(StmtKind::If { branches: vec![(given, vec![back])], otherwise: None }, at);
            let branch = (exits_by(exit, RETURNED, at), vec![then]);
            written.push(stmt(StmtKind::If { branches: vec![branch], otherwise: None }, at));
        }
        Some(false) => written.push(dispatch(exit, RETURNED, StmtKind::Return(None), at)),
        None => {}
    }
    if ways.broke {
        written.push(dispatch(exit, BROKE, StmtKind::Break, at));
    }
    if ways.continued {
        written.push(dispatch(exit, CONTINUED, StmtKind::Continue, at));
    }
    written
}

/// What's written after a source's body whose block always returns a value: `assert returned is
/// not None`, which narrows it, and `return returned`.
fn returned_always(returned: &str, at: Span) -> Vec<Stmt> {
    let none = Box::new(Expr { kind: ExprKind::None, span: here(at) });
    let given = Expr {
        kind: ExprKind::Binary {
            op: fennec_syntax::BinOp::IsNot,
            lhs: Box::new(name(returned, at)),
            rhs: none,
        },
        span: here(at),
    };
    vec![stmt(StmtKind::Assert(given), at), stmt(StmtKind::Return(Some(name(returned, at))), at)]
}

/// Whether every way through a block leaves it by `return` or `fail`.
fn always_returns(block: &[Stmt]) -> bool {
    let Some(last) = block.last() else { return false };
    match &last.kind {
        StmtKind::Return(_) | StmtKind::Fail(_) => true,
        StmtKind::If { branches, otherwise: Some(otherwise) } => {
            branches.iter().all(|(_, body)| always_returns(body)) && always_returns(otherwise)
        }
        StmtKind::With { body, .. } => always_returns(body),
        StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
            !arms.is_empty() && arms.iter().all(|arm| always_returns(&arm.body))
        }
        _ => false,
    }
}

/// `if exit == kind: then`.
fn dispatch(exit: &str, kind: i64, then: StmtKind, span: Span) -> Stmt {
    let branch = (exits_by(exit, kind, span), vec![stmt(then, span)]);
    stmt(StmtKind::If { branches: vec![branch], otherwise: None }, span)
}

/// Which ways out of the caller's block it takes: `returned` is whether it returns, and with a
/// value when it's `Some(true)`.
#[derive(Clone, Copy, Default)]
struct Ways {
    returned: Option<bool>,
    broke: bool,
    continued: bool,
}

impl Ways {
    fn any(self) -> bool {
        self.returned.is_some() || self.broke || self.continued
    }
}

/// What binding a source's parameters writes first, and the names they're renamed to.
type Bound = (Vec<Stmt>, Vec<(String, String)>);

/// What a `lend`'s value and `else:` block are written in as.
type LentAs<'a> = dyn FnMut(Expr, &[Stmt]) -> Option<Vec<Stmt>> + 'a;

struct Caller<'n> {
    exit: &'n str,
    returned: &'n str,
    ways: Ways,
    span: Span,
}

/// Replaces the `lend` in a body with what `with_block` makes of its value and `else:` block,
/// saying whether there was one.
fn replace_lend(body: &mut Vec<Stmt>, with_block: &mut LentAs<'_>) -> bool {
    for at in 0..body.len() {
        let Some(stmt) = body.get_mut(at) else { return false };
        if let StmtKind::Lend { value, otherwise } = &stmt.kind {
            let Some(written) = with_block(value.clone(), &otherwise.clone()) else { return false };
            body.splice(at..=at, written);
            return true;
        }
        let found = match &mut stmt.kind {
            StmtKind::If { branches, otherwise } => {
                branches.iter_mut().any(|(_, body)| replace_lend(body, with_block))
                    || otherwise.as_mut().is_some_and(|body| replace_lend(body, with_block))
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => replace_lend(body, with_block),
            StmtKind::Expr(value) | StmtKind::Assign { value, .. } => match &mut value.kind {
                ExprKind::Match { arms, .. } => {
                    arms.iter_mut().any(|arm| replace_lend(&mut arm.body, with_block))
                }
                _ => false,
            },
            _ => false,
        };
        if found {
            return true;
        }
    }
    false
}

/// The caller's block's `return`, `break` and `continue`, as setting `exit` and leaving the
/// loop it runs in. One inside a loop of the block's own leaves that loop, and then the block's.
fn exits(body: &mut Vec<Stmt>, caller: &mut Caller<'_>, in_loop: bool) {
    let mut at = 0;
    while at < body.len() {
        let Some(stmt) = body.get_mut(at) else { return };
        let span = caller.span;
        let exit = caller.exit;
        let set = |kind: i64| assign(exit, int(kind, span), false, None);
        // A way out written in an `else`, or another `match` inside the line, leaves the block too.
        // A loop's condition and what it goes through are its own, as its body is: a `break` there
        // is the loop's, not the block's.
        let own_loop = in_loop || matches!(stmt.kind, StmtKind::While { .. } | StmtKind::For { .. });
        for value in stmt_exprs(stmt) {
            outer_cases(value, &mut |cases| exits(cases, caller, own_loop));
        }
        let replaced = match &mut stmt.kind {
            StmtKind::Return(value) => {
                caller.ways.returned.get_or_insert(false);
                let mut written = Vec::new();
                if let Some(value) = value.take() {
                    caller.ways.returned = Some(true);
                    written.push(assign(caller.returned, value, false, None));
                }
                written.push(set(RETURNED));
                written.push(stmt_break(span));
                Some(written)
            }
            StmtKind::Break if !in_loop => {
                caller.ways.broke = true;
                Some(vec![set(BROKE), stmt_break(span)])
            }
            StmtKind::Continue if !in_loop => {
                caller.ways.continued = true;
                Some(vec![set(CONTINUED), stmt_break(span)])
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                let before = caller.ways.returned.is_some();
                exits(body, caller, true);
                // A `return` inside it leaves this loop too.
                (caller.ways.returned.is_some() && !before).then(|| {
                    let leave = dispatch(exit, RETURNED, StmtKind::Break, span);
                    vec![stmt.clone(), leave]
                })
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    exits(body, caller, in_loop);
                }
                if let Some(body) = otherwise {
                    exits(body, caller, in_loop);
                }
                None
            }
            StmtKind::With { body, .. } | StmtKind::OnFailure { body, .. } => {
                exits(body, caller, in_loop);
                None
            }
            _ => None,
        };
        match replaced {
            Some(written) => {
                let count = written.len();
                body.splice(at..=at, written);
                at += count;
            }
            None => at += 1,
        }
    }
}

/// Whether a block holds a `with` that isn't a `lock`, here or in a block inside, but not in a
/// function written inside.
fn holds_with(body: &[Stmt]) -> bool {
    body.iter().any(|each| match &each.kind {
        StmtKind::With { lock: false, .. } => true,
        StmtKind::With { body, .. }
        | StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::OnFailure { body, .. } => holds_with(body),
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| holds_with(body)) || otherwise.as_deref().is_some_and(holds_with)
        }
        StmtKind::Expr(value) | StmtKind::Assign { value, .. } => match &value.kind {
            ExprKind::Match { arms, .. } => arms.iter().any(|arm| holds_with(&arm.body)),
            _ => false,
        },
        _ => false,
    })
}

fn stmt_break(span: Span) -> Stmt {
    stmt(StmtKind::Break, span)
}

/// The expressions a statement is written with, but not those in the blocks inside it.
fn stmt_exprs(each: &mut Stmt) -> Vec<&mut Expr> {
    match &mut each.kind {
        StmtKind::Expr(value)
        | StmtKind::Return(Some(value))
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Fail(value)
        | StmtKind::While { cond: value, .. }
        | StmtKind::For { iter: value, .. }
        | StmtKind::With { value, .. }
        | StmtKind::Lend { value, .. } => vec![value],
        StmtKind::Assign { target, value, .. } => vec![target, value],
        StmtKind::If { branches, .. } => branches.iter_mut().map(|(cond, _)| cond).collect(),
        _ => Vec::new(),
    }
}

/// Calls `each` on the cases of every `match` in `value`, an `else`'s block among them, but not of
/// one inside another's case, which `each` reaches through that case, nor one in a lambda, which
/// leaves only the lambda.
///
/// The `match`es are told apart by their spans, worked out before `each` changes anything, so one
/// `each` adds isn't walked. A `match` with the same span as one inside its own case would be taken
/// as inner and left out: that needs a pass to copy one into its own case without new spans, which
/// none does.
fn outer_cases(value: &mut Expr, each: &mut dyn FnMut(&mut Vec<Stmt>)) {
    let mut inner = Vec::new();
    let mut outer = Vec::new();
    // Only read here, though the walk takes it to change.
    fennec_syntax::visit::in_expr(value, &mut |expr| match &mut expr.kind {
        ExprKind::Match { arms, .. } => {
            outer.push((expr.span.start, expr.span.end));
            for arm in arms {
                fennec_syntax::visit::in_stmts(&mut arm.body, &mut |deeper| {
                    if matches!(deeper.kind, ExprKind::Match { .. }) {
                        inner.push((deeper.span.start, deeper.span.end));
                    }
                });
            }
        }
        ExprKind::Lambda { body, .. } => {
            fennec_syntax::visit::in_expr(body, &mut |deeper| {
                if matches!(deeper.kind, ExprKind::Match { .. }) {
                    inner.push((deeper.span.start, deeper.span.end));
                }
            });
        }
        _ => {}
    });
    outer.retain(|span| !inner.contains(span));
    if outer.is_empty() {
        return;
    }
    fennec_syntax::visit::in_expr(value, &mut |expr| {
        let at = (expr.span.start, expr.span.end);
        if let ExprKind::Match { arms, .. } = &mut expr.kind
            && let Some(found) = outer.iter().position(|span| *span == at)
        {
            outer.swap_remove(found);
            for arm in arms {
                each(&mut arm.body);
            }
        }
    });
}

/// Each way the caller's block can fail, a `fail` or a `?`, runs `otherwise` first.
fn fail_through(body: &mut Vec<Stmt>, otherwise: &[Stmt]) {
    let mut at = 0;
    while at < body.len() {
        let Some(stmt) = body.get_mut(at) else { return };
        // A function written inside fails by itself, not the block.
        if matches!(stmt.kind, StmtKind::Def(_)) {
            at += 1;
            continue;
        }
        let mut before = Vec::new();
        let mut made = 0;
        // A whole line that's a `?` becomes a `match` whose failing case runs the cleanup
        // already: its cases aren't the program's, so they're not walked again.
        if hoist_tries(stmt, otherwise, &mut before, &mut made) {
            let count = before.len();
            body.splice(at..at, before);
            at += count + 1;
            continue;
        }
        // A `fail` written in an `else`, or another `match` inside the line, fails the block too.
        for value in stmt_exprs(stmt) {
            outer_cases(value, &mut |cases| fail_through(cases, otherwise));
        }
        match &mut stmt.kind {
            StmtKind::Fail(error) if otherwise.iter().any(cleanup_fails) => {
                // The block's failure, held to take the cleanup's as notes, fails it after.
                let span = stmt.span;
                let mut failing = failing_with(error.clone(), otherwise, span);
                if let Some(last) = failing.pop() {
                    stmt.kind = last.kind;
                }
                before.extend(failing);
            }
            StmtKind::Fail(_) => {
                before.extend(otherwise.iter().cloned());
            }
            StmtKind::If { branches, otherwise: rest } => {
                for (_, body) in branches {
                    fail_through(body, otherwise);
                }
                if let Some(body) = rest {
                    fail_through(body, otherwise);
                }
            }
            // A round's failure leaves where the loop ends, and a round can't write outside itself.
            StmtKind::For { iter, .. } if crate::parallel::parallel_items(iter).is_some() => {}
            StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
                fail_through(body, otherwise);
            }
            StmtKind::OnFailure { cleanup, body, .. } => {
                fail_through(cleanup, otherwise);
                fail_through(body, otherwise);
            }
            _ => {}
        }
        let count = before.len();
        body.splice(at..at, before);
        at += count + 1;
    }
}

/// Takes each `?` in what a statement runs before its blocks out to its own `match` before it,
/// which runs `otherwise` before failing: `x = f()?` becomes
/// `got = match f(): case Ok(v): v / case Err(e): otherwise; fail e`, then `x = got`.
fn hoist_tries(held: &mut Stmt, otherwise: &[Stmt], before: &mut Vec<Stmt>, made: &mut usize) -> bool {
    // A whole line that's a `?`, like `file.write_text(text)?`, is a `match` statement: what it
    // gives back may be nothing, which no name holds.
    if let StmtKind::Expr(Expr { kind: ExprKind::Try { value, context: None }, .. }) = &held.kind {
        let span = held.span;
        let ok = MatchArm {
            pattern: Pattern::Variant {
                name: "Ok".to_owned(),
                dotted: false,
                args: vec![Pattern::Wildcard],
                fields: Vec::new(),
            },
            guard: None,
            body: vec![stmt(StmtKind::Pass, span)],
            span: here(span),
        };
        let matched = Expr {
            kind: ExprKind::Match { subject: value.clone(), arms: vec![ok, failed_arm(otherwise, span)] },
            span: here(span),
        };
        held.kind = StmtKind::Expr(matched);
        return true;
    }
    loop {
        let mut taken = None;
        let span = held.span;
        let at = *made;
        let mut take = |expr: &mut Expr| {
            if taken.is_none()
                && let ExprKind::Try { value, context: None } = &expr.kind
            {
                // A span of its own, as each written-in copy of a source gets the same `?` at the
                // same place, and two copies in one block would declare one name twice.
                let got = format!("got__try{}_{at}", here(span).start);
                taken = Some((got.clone(), value.as_ref().clone()));
                expr.kind = ExprKind::Name(got);
            }
        };
        match &mut held.kind {
            StmtKind::Expr(value)
            | StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value)
            | StmtKind::Return(Some(value))
            | StmtKind::For { iter: value, .. }
            | StmtKind::With { value, .. } => outside_functions(value, &mut take),
            StmtKind::Assign { target, value, .. } => {
                outside_functions(value, &mut take);
                outside_functions(target, &mut take);
            }
            StmtKind::If { branches, .. } => {
                if let Some((cond, _)) = branches.first_mut() {
                    outside_functions(cond, &mut take);
                }
            }
            _ => {}
        }
        let Some((got, value)) = taken else { return false };
        *made += 1;
        let ok = arm("Ok", "value__try", vec![stmt_value(name("value__try", span))], span);
        let matched = Expr {
            kind: ExprKind::Match { subject: Box::new(value), arms: vec![ok, failed_arm(otherwise, span)] },
            span: here(span),
        };
        let mut hoisted = assign(&got, matched, false, None);
        // What was taken out may hold `?`s of its own, which go before it.
        let mut inner = Vec::new();
        let _ = hoist_tries(&mut hoisted, otherwise, &mut inner, made);
        before.extend(inner);
        before.push(hoisted);
    }
}

/// A hoisted `?`'s `Err` case: the `lend`'s `else:`, then the failure passes on.
fn failed_arm(otherwise: &[Stmt], span: Span) -> MatchArm {
    let failed = failing_with(name("error__try", span), otherwise, span);
    arm("Err", "error__try", failed, span)
}

/// The block failing with `error`, once `otherwise`, the `lend`'s `else:`, has run. A failure of
/// the cleanup itself is added to the block's as a note, in its printed form, and the rest of the
/// cleanup is skipped, so the block's own failure is the one that passes up.
fn failing_with(error: Expr, otherwise: &[Stmt], span: Span) -> Vec<Stmt> {
    if !otherwise.iter().any(cleanup_fails) {
        let mut failed = otherwise.to_vec();
        failed.push(stmt(StmtKind::Fail(error), span));
        return failed;
    }
    let held = format!("failed__noted{}", here(span).start);
    let mut cleanup = noted(otherwise, &held, span);
    cleanup.push(stmt(StmtKind::Break, span));
    let always = Expr { kind: ExprKind::Bool(true), span: here(span) };
    vec![
        assign(&held, error, true, None),
        stmt(StmtKind::While { cond: always, body: cleanup }, span),
        stmt(StmtKind::Fail(name(&held, span)), span),
    ]
}

/// Whether a cleanup statement can fail: a `fail`, or a whole line that's a `?`, here or in an
/// `if` inside it.
fn cleanup_fails(each: &Stmt) -> bool {
    match &each.kind {
        StmtKind::Fail(_)
        | StmtKind::Expr(Expr { kind: ExprKind::Try { context: None, .. }, .. })
        | StmtKind::Assign {
            value: Expr { kind: ExprKind::Try { context: None, .. }, .. }, op: None, ..
        } => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| body.iter().any(cleanup_fails))
                || otherwise.iter().flatten().any(cleanup_fails)
        }
        _ => false,
    }
}

/// A cleanup with each of its failures added to `held` as a note, and the rest of it skipped by
/// leaving the loop it's run in.
fn noted(cleanup: &[Stmt], held: &str, span: Span) -> Vec<Stmt> {
    let note = |failure: Expr| note_on(held, failure, span);
    let mut written = Vec::new();
    for (at, each) in cleanup.iter().enumerate() {
        match &each.kind {
            StmtKind::Fail(failure) => {
                written.push(note(failure.clone()));
                written.push(stmt(StmtKind::Break, span));
            }
            StmtKind::Expr(Expr { kind: ExprKind::Try { value, context: None }, .. }) => {
                let ok = MatchArm {
                    pattern: Pattern::Variant {
                        name: "Ok".to_owned(),
                        dotted: false,
                        args: vec![Pattern::Wildcard],
                        fields: Vec::new(),
                    },
                    guard: None,
                    body: vec![stmt(StmtKind::Pass, span)],
                    span: here(span),
                };
                let cut = format!("cleanup__failed{}", here(span).start);
                let failed =
                    arm("Err", &cut, vec![note(name(&cut, span)), stmt(StmtKind::Break, span)], span);
                let matched = Expr {
                    kind: ExprKind::Match { subject: value.clone(), arms: vec![ok, failed] },
                    span: here(span),
                };
                written.push(stmt_value(matched));
            }
            // `x = f()?`: `x` takes the value and the rest goes on with it, or the failure is a
            // note and the rest is skipped.
            StmtKind::Assign {
                target,
                op: None,
                value: Expr { kind: ExprKind::Try { value, context: None }, .. },
                declare_mut,
                ty,
            } => {
                let got = format!("cleanup__got{}", here(span).start);
                let kept = StmtKind::Assign {
                    target: target.clone(),
                    op: None,
                    value: name(&got, span),
                    declare_mut: *declare_mut,
                    ty: ty.clone(),
                };
                let mut rest = vec![stmt(kept, each.span)];
                rest.extend(noted(cleanup.get(at + 1..).unwrap_or_default(), held, span));
                let ok = arm("Ok", &got, rest, span);
                let cut = format!("cleanup__failed{}", here(span).start);
                let failed =
                    arm("Err", &cut, vec![note(name(&cut, span)), stmt(StmtKind::Break, span)], span);
                let matched = Expr {
                    kind: ExprKind::Match { subject: value.clone(), arms: vec![ok, failed] },
                    span: here(span),
                };
                written.push(stmt_value(matched));
                return written;
            }
            StmtKind::If { branches, otherwise } => {
                let branches =
                    branches.iter().map(|(cond, body)| (cond.clone(), noted(body, held, span))).collect();
                let otherwise = otherwise.as_ref().map(|body| noted(body, held, span));
                written.push(Stmt { kind: StmtKind::If { branches, otherwise }, span: each.span });
            }
            _ => written.push(each.clone()),
        }
    }
    written
}

/// `held.add_note(f"{failure}")`: a cleanup's failure, as a note on the block's.
fn note_on(held: &str, failure: Expr, span: Span) -> Stmt {
    let text = Expr {
        kind: ExprKind::FStr(vec![fennec_syntax::FStrPart::Expr { expr: failure, spec: None }]),
        span: here(span),
    };
    let callee = Expr {
        kind: ExprKind::Attr {
            object: Box::new(name(held, span)),
            name: "add_note".to_owned(),
            optional: false,
        },
        span: here(span),
    };
    let args = vec![fennec_syntax::Arg { name: None, is_mut: false, value: text }];
    stmt_value(Expr { kind: ExprKind::Call { callee: Box::new(callee), args }, span: here(span) })
}

/// The block the caller wrote, given what's lent: by its name, or split into several names.
fn given_as(block: &mut Vec<Stmt>, (split, names): (bool, &[WithName]), held: &str, place: &str, at: Span) {
    if split {
        let targets = names.iter().map(|named| name(&named.name, named.span)).collect();
        let target = Expr { kind: ExprKind::Tuple(targets), span: here(at) };
        let kind = StmtKind::Assign { target, op: None, value: name(place, at), declare_mut: true, ty: None };
        block.insert(0, stmt(kind, at));
    } else {
        rename(block, &[(held.to_owned(), place.to_owned())]);
    }
}

/// A cleanup that unmarks what a crash runs before it runs, when there's one.
fn unmarked_first(crash: Option<&(Stmt, Stmt, Stmt)>, otherwise: &[Stmt]) -> Vec<Stmt> {
    match crash {
        Some((_, _, done)) => std::iter::once(done.clone()).chain(otherwise.iter().cloned()).collect(),
        None => otherwise.to_vec(),
    }
}

/// A source's body as `with src else fail E:` writes it in: each `fail` does what the `else` does
/// instead, and so does each `?`, through `else`. A `?` in an arrow function passes the arrow's
/// own failure up, not the source's, so it stays.
fn fail_instead(body: &mut [Stmt], instead: &Expr) {
    instead_of_fails(body, instead);
    let leaving = Box::new(instead.clone());
    let mut in_arrows = std::collections::BTreeSet::new();
    fennec_syntax::visit::in_stmts(body, &mut |expr| {
        if let ExprKind::Lambda { body, .. } = &mut expr.kind {
            fennec_syntax::visit::in_expr(body, &mut |inner| {
                if matches!(inner.kind, ExprKind::Try { .. }) {
                    in_arrows.insert((inner.span.start, inner.span.end));
                }
            });
        }
    });
    fennec_syntax::visit::in_stmts(body, &mut |expr| {
        if in_arrows.contains(&(expr.span.start, expr.span.end)) {
            return;
        }
        match &mut expr.kind {
            ExprKind::Try { value, context: None } => {
                let kind = ExprKind::Else { value: value.clone(), default: leaving.clone() };
                expr.kind = kind;
            }
            // `value else fail E` fails as the source does, so with what's said instead.
            ExprKind::Else { default, .. } if fails_out(default) => default.clone_from(&leaving),
            _ => {}
        }
    });
}

/// Whether an `else`'s default is `fail E`: it leaves, with a `fail`.
fn fails_out(default: &Expr) -> bool {
    let ExprKind::Match { arms, .. } = &default.kind else { return false };
    default.leaves()
        && arms
            .first()
            .and_then(|arm| arm.body.last())
            .is_some_and(|last| matches!(last.kind, StmtKind::Fail(_)))
}

/// Each `fail` in a source's body, inside what's in it but not a function of its own, made to do
/// what `instead` does.
fn instead_of_fails(body: &mut [Stmt], instead: &Expr) {
    for each in body {
        match &mut each.kind {
            StmtKind::Fail(_) => each.kind = StmtKind::Expr(instead.clone()),
            StmtKind::If { branches, otherwise } => {
                for (_, block) in branches {
                    instead_of_fails(block, instead);
                }
                if let Some(block) = otherwise {
                    instead_of_fails(block, instead);
                }
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
                instead_of_fails(body, instead);
            }
            StmtKind::Lend { otherwise, .. } => instead_of_fails(otherwise, instead),
            StmtKind::Expr(value) | StmtKind::Assign { value, .. } => {
                if let ExprKind::Match { arms, .. } = &mut value.kind {
                    for arm in arms {
                        instead_of_fails(&mut arm.body, instead);
                    }
                }
            }
            _ => {}
        }
    }
}

/// What starts the name of the function a crash in a `with` block runs.
pub(crate) const ON_CRASH: &str = "crash__cleanup";

/// What a crash in a `with` block runs: the source's `else:` as a function taking nothing, the
/// call that marks it to run, and the call that unmarks it. On a crash there's no failure to pass
/// up, so a whole line's `?` in it only runs, and a `fail` ends it. `None` with no `else:`, or
/// with an `else:` that passes a failure up some other way.
fn on_crash(otherwise: &[Stmt], at: Span) -> Option<(Stmt, Stmt, Stmt)> {
    if otherwise.is_empty() {
        return None;
    }
    let body = quietly(otherwise)?;
    let held = format!("{ON_CRASH}{}", here(at).start);
    let def = FnDef {
        name: held.clone(),
        generics: Vec::new(),
        params: Vec::new(),
        ret: None,
        arrow: None,
        fails: None,
        body,
        annotations: Vec::new(),
        span: here(at),
    };
    let call = |intrinsic: &str| {
        let callee = Box::new(name(&format!("intrinsics.{intrinsic}"), at));
        let given = fennec_syntax::Arg { name: None, is_mut: false, value: name(&held, at) };
        stmt_value(Expr { kind: ExprKind::Call { callee, args: vec![given] }, span: here(at) })
    };
    Some((stmt(StmtKind::Def(def), at), call("crash_cleanup"), call("crash_cleanup_done")))
}

/// A cleanup as a crash runs it, with nothing to pass up: a whole line that's a `?` runs and its
/// failure is dropped, and a `fail` ends it. `None` when a failure passes up any other way.
fn quietly(cleanup: &[Stmt]) -> Option<Vec<Stmt>> {
    let mut quiet = Vec::new();
    for each in cleanup {
        let kind = match &each.kind {
            StmtKind::Fail(_) => StmtKind::Return(None),
            StmtKind::Expr(Expr { kind: ExprKind::Try { value, context: None }, .. }) => {
                let pattern = Pattern::Wildcard;
                let pass = stmt(StmtKind::Pass, each.span);
                let arm = MatchArm { pattern, guard: None, body: vec![pass], span: each.span };
                let kind = ExprKind::Match { subject: value.clone(), arms: vec![arm] };
                StmtKind::Expr(Expr { kind, span: here(each.span) })
            }
            StmtKind::If { branches, otherwise } => {
                let branches = branches
                    .iter()
                    .map(|(cond, body)| Some((cond.clone(), quietly(body)?)))
                    .collect::<Option<Vec<_>>>()?;
                let otherwise = match otherwise {
                    Some(body) => Some(quietly(body)?),
                    None => None,
                };
                StmtKind::If { branches, otherwise }
            }
            _ if passes_up(each) => return None,
            other => other.clone(),
        };
        quiet.push(Stmt { kind, span: each.span });
    }
    Some(quiet)
}

/// Whether a statement has a `?` or a `fail` anywhere in it.
fn passes_up(each: &Stmt) -> bool {
    let mut found = false;
    let mut held = vec![each.clone()];
    fennec_syntax::visit::in_stmts(&mut held, &mut |expr| {
        if matches!(expr.kind, ExprKind::Try { .. }) {
            found = true;
        }
    });
    found
        || blocks_of(each).iter().any(|block| block.iter().any(passes_up))
        || matches!(each.kind, StmtKind::Fail(_))
}

/// The blocks directly inside a statement.
fn blocks_of(each: &Stmt) -> Vec<&[Stmt]> {
    match &each.kind {
        StmtKind::If { branches, otherwise } => {
            branches.iter().map(|(_, body)| body.as_slice()).chain(otherwise.as_deref()).collect()
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => vec![body],
        _ => Vec::new(),
    }
}

/// Writes `if intrinsics.abandoning(): return` after each `yield` in `body`, but not in what's
/// defined there, and not twice, as a source inside another's block asks too.
fn leave_when_abandoned(body: &mut Vec<Stmt>, at: Span) {
    let asks = |stmt: &Stmt| {
        matches!(&stmt.kind, StmtKind::If { branches, .. } if branches.first().is_some_and(|(cond, _)| {
            matches!(&cond.kind, ExprKind::Call { callee, .. }
                if matches!(&callee.kind, ExprKind::Name(called) if called == ABANDONING))
        }))
    };
    let mut index = 0;
    while index < body.len() {
        if let Some(stmt) = body.get_mut(index) {
            match &mut stmt.kind {
                StmtKind::If { branches, otherwise } => {
                    for (_, block) in branches {
                        leave_when_abandoned(block, at);
                    }
                    if let Some(block) = otherwise {
                        leave_when_abandoned(block, at);
                    }
                }
                StmtKind::While { body: block, .. }
                | StmtKind::For { body: block, .. }
                | StmtKind::With { body: block, .. }
                | StmtKind::OnFailure { body: block, .. } => leave_when_abandoned(block, at),
                StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
                    for arm in arms {
                        leave_when_abandoned(&mut arm.body, at);
                    }
                }
                _ => {}
            }
        }
        let yielded = body.get(index).is_some_and(|stmt| matches!(stmt.kind, StmtKind::Yield(_)));
        if yielded && !body.get(index + 1).is_some_and(asks) {
            let callee = Box::new(name(ABANDONING, at));
            let asked = Expr { kind: ExprKind::Call { callee, args: Vec::new() }, span: here(at) };
            let leave = vec![stmt(StmtKind::Return(None), at)];
            body.insert(
                index + 1,
                stmt(StmtKind::If { branches: vec![(asked, leave)], otherwise: None }, at),
            );
            index += 1;
        }
        index += 1;
    }
}

/// What a generator asks after a `yield` in a source's block: whether it was let go there.
const ABANDONING: &str = "intrinsics.abandoning";

fn stmt_value(value: Expr) -> Stmt {
    let span = value.span;
    Stmt { kind: StmtKind::Expr(value), span }
}

fn arm(variant: &str, bound: &str, body: Vec<Stmt>, span: Span) -> MatchArm {
    let pattern = Pattern::Variant {
        name: variant.to_owned(),
        dotted: false,
        args: vec![Pattern::Bind(bound.to_owned())],
        fields: Vec::new(),
    };
    MatchArm { pattern, guard: None, body, span: here(span) }
}

/// Calls `change` on each expression in `value` outside the arrow functions, comprehensions and
/// `match` arms in it, whose `?`s don't fail the block where they're written.
fn outside_functions(value: &mut Expr, change: &mut dyn FnMut(&mut Expr)) {
    change(value);
    match &mut value.kind {
        ExprKind::Call { callee, args } => {
            outside_functions(callee, change);
            for arg in args {
                outside_functions(&mut arg.value, change);
            }
        }
        ExprKind::Attr { object, .. } => outside_functions(object, change),
        ExprKind::Index { object, index } => {
            outside_functions(object, change);
            outside_functions(index, change);
        }
        // What `and` and `or` may not run stays where it is: the checker allows no `?` there.
        ExprKind::Binary { op: fennec_syntax::BinOp::And | fennec_syntax::BinOp::Or, lhs, .. } => {
            outside_functions(lhs, change);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            outside_functions(lhs, change);
            outside_functions(rhs, change);
        }
        ExprKind::FStr(parts) => {
            for part in parts {
                if let fennec_syntax::FStrPart::Expr { expr, .. } = part {
                    outside_functions(expr, change);
                }
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, value) in pairs {
                outside_functions(key, change);
                outside_functions(value, change);
            }
        }
        ExprKind::Unary { expr, .. } => outside_functions(expr, change),
        ExprKind::Try { value, .. } => outside_functions(value, change),
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                outside_functions(item, change);
            }
        }
        _ => {}
    }
}

/// The names a body declares: what it assigns, and what its loops and `with`s name.
fn declared_in(body: &[Stmt], found: &mut Vec<String>) {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Assign { target, value, .. } => {
                targets(target, found);
                declared_in_match(value, found);
            }
            StmtKind::For { names, body, .. } => {
                found.extend(names.iter().cloned());
                declared_in(body, found);
            }
            StmtKind::With { names, body, .. } => {
                found.extend(names.iter().map(|held| held.name.clone()));
                declared_in(body, found);
            }
            StmtKind::While { body, .. } => declared_in(body, found),
            StmtKind::OnFailure { cleanup, body, .. } => {
                declared_in(cleanup, found);
                declared_in(body, found);
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    declared_in(body, found);
                }
                if let Some(body) = otherwise {
                    declared_in(body, found);
                }
            }
            StmtKind::Lend { otherwise, .. } => declared_in(otherwise, found),
            StmtKind::Expr(value) => declared_in_match(value, found),
            _ => {}
        }
    }
}

/// What a `match` written as a whole value declares: what its cases bind, and what they declare.
fn declared_in_match(value: &Expr, found: &mut Vec<String>) {
    if let ExprKind::Match { arms, .. } = &value.kind {
        for arm in arms {
            bound_by(&mut arm.pattern.clone(), &mut |held| found.push(held.clone()));
            declared_in(&arm.body, found);
        }
    }
}

fn targets(target: &Expr, found: &mut Vec<String>) {
    match &target.kind {
        // `_ = value` throws the value away, so it declares nothing to rename, kept apart from
        // the others as `_'1` or as written.
        ExprKind::Name(name) if name != "_" && !name.starts_with("_'") => found.push(name.clone()),
        ExprKind::Tuple(items) => items.iter().for_each(|item| targets(item, found)),
        _ => {}
    }
}

/// Renames names in a body, those it reads and those its loops and `with`s give.
fn rename(body: &mut [Stmt], renamed: &[(String, String)]) {
    let fresh = |name: &str| renamed.iter().find(|(from, _)| from == name).map(|(_, to)| to.clone());
    fennec_syntax::visit::in_stmts(body, &mut |expr| match &mut expr.kind {
        ExprKind::Name(name) => {
            if let Some(to) = fresh(name) {
                *name = to;
            }
        }
        ExprKind::Lambda { params, .. } => {
            for param in params {
                if let Some(to) = fresh(param) {
                    *param = to;
                }
            }
        }
        ExprKind::Comprehension { names, .. } => {
            for held in names {
                if let Some(to) = fresh(held) {
                    *held = to;
                }
            }
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                bound_by(&mut arm.pattern, &mut |held| {
                    if let Some(to) = fresh(held) {
                        *held = to;
                    }
                });
            }
        }
        _ => {}
    });
    rename_bound(body, &fresh);
}

/// Each name a pattern binds.
fn bound_by(pattern: &mut Pattern, each: &mut dyn FnMut(&mut String)) {
    match pattern {
        Pattern::Bind(held) | Pattern::RefBind(held) => each(held),
        Pattern::Variant { args: patterns, .. } | Pattern::Tuple(patterns) | Pattern::Or(patterns) => {
            for inner in patterns {
                bound_by(inner, each);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}

fn rename_bound(body: &mut [Stmt], fresh: &dyn Fn(&str) -> Option<String>) {
    for stmt in body {
        match &mut stmt.kind {
            StmtKind::For { names, body, .. } => {
                for held in names {
                    if let Some(to) = fresh(held) {
                        *held = to;
                    }
                }
                rename_bound(body, fresh);
            }
            StmtKind::With { names, body, .. } => {
                for held in names {
                    if let Some(to) = fresh(&held.name) {
                        held.name = to;
                    }
                }
                rename_bound(body, fresh);
            }
            StmtKind::While { body, .. } => rename_bound(body, fresh),
            StmtKind::OnFailure { cleanup, body, .. } => {
                rename_bound(cleanup, fresh);
                rename_bound(body, fresh);
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    rename_bound(body, fresh);
                }
                if let Some(body) = otherwise {
                    rename_bound(body, fresh);
                }
            }
            StmtKind::Lend { otherwise, .. } => rename_bound(otherwise, fresh),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    /// A pass that writes nothing in says so, however many passes came before it: lowering
    /// stops once nothing's left to write in, rather than checking and lowering again for ever,
    /// as it did when a standard library source holding a `with` of another stayed in what was
    /// checked.
    #[test]
    fn a_pass_that_writes_nothing_in_says_so() {
        let source = "error Boom:\n    pass\n\n\
            def opened(n: int) -> with int fails Boom:\n    lend n else:\n        pass\n\n\
            def main(stdout: Stdout) fails Boom:\n    with opened(1)? as first:\n        stdout.print(first)\n";
        let mut module = fennec_syntax::parse(source).unwrap();
        let checked = crate::checker::Checker::new(&module).run(crate::checker::Scope::Everything).unwrap();
        let (launches, matches) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
        assert!(super::lower(&mut module, &checked.sources, &launches, &matches));
        assert!(!super::lower(&mut module, &checked.sources, &launches, &matches));
    }

    /// A generic source's type parameter, written in as a type naming the caller's own, like a
    /// `channel[T]` opened as `channel[Box[T]]`, is written in once: the `T` inside isn't taken
    /// for the source's again, which wrote it in without end and took memory with it.
    #[test]
    fn a_type_written_in_for_a_sources_parameter_is_written_once() {
        let mut block = vec![super::stmt(
            fennec_syntax::StmtKind::Expr(super::name("T", fennec_syntax::Span::default())),
            fennec_syntax::Span::default(),
        )];
        let boxed = fennec_syntax::parse("x = Box[T]\n").unwrap();
        let Some(fennec_syntax::Item::Const { value, .. }) = boxed.items.first() else {
            panic!("no constant in {boxed:?}");
        };
        let written = fennec_syntax::TypeExpr {
            name: "Box".to_owned(),
            args: vec![fennec_syntax::TypeExpr {
                name: "T".to_owned(),
                args: Vec::new(),
                optional: false,
                span: fennec_syntax::Span::default(),
            }],
            optional: false,
            span: fennec_syntax::Span::default(),
        };
        let given = std::collections::BTreeMap::from([("T".to_owned(), (written, value.clone()))]);
        super::retype(&mut block, &given);
        let Some(fennec_syntax::StmtKind::Expr(retyped)) = block.first().map(|stmt| &stmt.kind) else {
            panic!("no expression in {block:?}");
        };
        assert_eq!(retyped.kind, value.kind);
    }
}
