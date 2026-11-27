//! Writes each `with` over a source in as the source's own body, once a program checks, so
//! every engine runs it with nothing new. `with transaction()? as tx:` and its block become the
//! source's statements, its parameters bound to the arguments and its names hidden, with the
//! `lend` replaced by `tx = ...` and the caller's block:
//!
//! - The block's `return`, `break` and `continue` leave a loop that runs it once, and are taken
//!   again after the source's cleanup, so cleanup below the `lend` always runs on success.
//! - Each `fail` and `?` in the block leaves the same loop with the failure held, and the
//!   `lend`'s `else:` runs once after it, before the failure passes up, so it runs whenever the
//!   caller's block fails. A `?` is taken out to its own `match` on the result before its
//!   statement. Written once, the `else:` of a `with` inside another is one failure for the outer
//!   one to clean up after, so what's written grows with how deep they nest, not doubling.
//!
//! It's the only place that knows how a source runs, so a native one can replace it alone.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use fennec_syntax::sight::Binder;
use fennec_syntax::{
    Copied, Expr, ExprKind, FnDef, Item, MatchArm, Module, Pattern, Span, Stmt, StmtKind, TypeExpr, WithName,
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
    /// A `with launch(...)`'s task, written in where its placeholder is once the source's own
    /// names are hidden, so its own are the caller's.
    pub(crate) task: Option<crate::launch::Task>,
}

/// The fields each `match` with `ref` cases binds by name, by its case, found by the `match`'s span.
type RefMatches = BTreeMap<(u32, u32), Vec<Option<Vec<String>>>>;

/// Writes in every `with` over a source in `module`, saying whether there were any.
pub(crate) fn lower(
    module: &mut Module,
    sources: &BTreeMap<(u32, u32), Source>,
    launches: &BTreeMap<(u32, u32), crate::launch::Launch>,
    (ref_matches, test_fails): (&RefMatches, &BTreeMap<(u32, u32), TypeExpr>),
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
    NEXT.with(|next| next.set(below_copies(written_low) - 1));
    COPIES.with(|copies| copies.set(lowest));
    let mut lowering = Lowering {
        sources,
        launches,
        ref_matches,
        made,
        returns: Vec::new(),
        failing: Vec::new(),
        fails: Vec::new(),
        instead: None,
        binds: Vec::new(),
        tails: BTreeSet::new(),
    };
    for item in &mut module.items {
        match item {
            Item::Function(def) => lowering.function(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                def.methods.iter_mut().for_each(|def| lowering.function(def));
            }
            Item::Extend(extension) => extension.methods.iter_mut().for_each(|def| lowering.function(def)),
            Item::Test(test) => {
                // A test that holds a source's failure says what it fails with, worked out from its
                // body, so it holds the failure as a function does: the source's cleanup runs once,
                // after what's inside the block is left (#2040).
                if test.fails.is_none()
                    && holds_with(&test.body)
                    && let Some(fails) = test_fails.get(&(test.span.start, test.span.end))
                {
                    test.fails = Some(fails.clone());
                }
                lowering.returns.push(None);
                lowering.failing.push(test.fails.is_some());
                lowering.fails.push(test.fails.clone());
                let given: Vec<String> =
                    test.capabilities.iter().chain(&test.inputs).map(|param| param.name.clone()).collect();
                lowering.binds.push(fennec_syntax::sight::bound_in(&test.body, &given));
                lowering.stmts(&mut test.body);
                lowering.binds.pop();
                lowering.fails.pop();
                lowering.failing.pop();
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
pub(crate) fn nest(module: &mut Module, opened_apart: &BTreeSet<(u32, u32)>) -> bool {
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

fn nest_in(body: &mut [Stmt], opened_apart: &BTreeSet<(u32, u32)>, nested: &mut bool) {
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
    /// What each function being walked says it fails with, innermost last.
    fails: Vec<Option<TypeExpr>>,
    /// What the source being written in does in place of failing, for `with src else fail E:`.
    instead: Option<Expr>,
    /// The names each function being walked binds anywhere, its parameters among them, innermost
    /// last: a source written in there calls a function of its own by one of these through a mark.
    binds: Vec<BTreeSet<String>>,
    /// The `with`s that are the last line of a function giving back a value, by span, through
    /// `if`s and `match`es: one whose block always fails says so where it's written in.
    tails: BTreeSet<(u32, u32)>,
}

thread_local! {
    /// The next span for something a lowering writes, counting down from just below the
    /// standard library's, where no program's text reaches.
    static NEXT: std::cell::Cell<u32> = const { std::cell::Cell::new(Span::PRELUDE - 1) };
    /// Where the next copy of a generic source's text goes, counting down from the top: still
    /// the standard library's, so it keeps its own privacy, but past where its text reaches.
    static COPIES: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
    /// Where each copy of the program's text written in so far went, as `Module::copied` holds it,
    /// so a copy of text inside an earlier copy is noted with the text too.
    static COPIED: std::cell::RefCell<Vec<Copied>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Each span a copy written in this round moved, and where to, in the order they moved: what
    /// the check found at the text is found at the copy too, for what's lowered later in the round.
    static MOVED: std::cell::RefCell<Vec<Move>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Copies just made, noted in `COPIED` once it's no longer being read.
    static PENDING: std::cell::RefCell<Vec<Copied>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The next span for a copy of the program's text that stays the program's, counting down
    /// from just below the spans lowering writes, and the lowest it can be, past the program's text.
    static PROGRAM_COPIES: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((WRITTEN_FROM - 1, 0)) };
    /// Whether a copy among the program's text found no room this round.
    static PROGRAM_COPIES_RAN_OUT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether a copy of the standard library's text found no room this round.
    static STD_COPIES_RAN_OUT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A span a copy moved, as its start and end, and where to.
type Move = ((u32, u32), (u32, u32));

/// Starts a round's writing in with the copies `module` holds, the next copy that stays the
/// program's text below every one of them, and above all of the program's own text.
pub(crate) fn copies_from(module: &mut Module) {
    let copied = module.copied.clone();
    let top = copied.iter().map(|copy| copy.at).filter(|at| *at < WRITTEN_FROM).min().unwrap_or(WRITTEN_FROM);
    let mut text_end = module.files.iter().map(|file| file.end).max().unwrap_or(0);
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        if expr.span.end < top {
            text_end = text_end.max(expr.span.end);
        }
    });
    // Statements end past their last expression, as a `pass` does, but never this far.
    let floor = text_end.saturating_add(1 << 20);
    PROGRAM_COPIES.with(|next| next.set((top.saturating_sub(1), floor)));
    PROGRAM_COPIES_RAN_OUT.with(|ran_out| ran_out.set(false));
    STD_COPIES_RAN_OUT.with(|ran_out| ran_out.set(false));
    COPIED.with(|held| *held.borrow_mut() = copied);
    MOVED.with(|moved| moved.borrow_mut().clear());
}

/// What a check found by span, found too at each span a copy written in this round moved its
/// text's to, so a lowering later in the round reads a copy as it would the text it copies: a
/// `shared` value a source declares is lowered in each copy with its `lock`s.
pub(crate) fn found_in_copies<V: Clone>(
    found: &BTreeMap<(u32, u32), V>,
) -> std::borrow::Cow<'_, BTreeMap<(u32, u32), V>> {
    MOVED.with(|moved| {
        let moved = moved.borrow();
        if found.is_empty() || moved.is_empty() {
            return std::borrow::Cow::Borrowed(found);
        }
        let mut all = found.clone();
        for (from, to) in moved.iter() {
            if let Some(value) = all.get(from).cloned() {
                all.insert(*to, value);
            }
        }
        std::borrow::Cow::Owned(all)
    })
}

/// Where a copy goes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Region {
    /// Among the program's text, which every check of whether something is the program's own reads
    /// as its own: a source's body written in where it's used.
    Program,
    /// Among what lowering writes in.
    Written,
}

/// Reserves `length` spans in a row in `region` past the last, giving the lowest. Nothing, when
/// the program's have run out.
fn reserved_in(region: Region, length: u32) -> Option<u32> {
    match region {
        Region::Written => Some(reserved(length)),
        Region::Program => PROGRAM_COPIES.with(|next| {
            let (top, floor) = next.get();
            let Some(low) = top.checked_sub(length).filter(|low| *low > floor) else {
                PROGRAM_COPIES_RAN_OUT.with(|ran_out| ran_out.set(true));
                return None;
            };
            next.set((low - 1, floor));
            Some(low)
        }),
    }
}

/// Whether a copy among the program's text found no room this round: the program's own text,
/// each file in a slot of its own, left too little below what the compiler writes in. What was
/// written in then can't be told apart where it must be, so the round refuses the program.
pub(crate) fn program_copies_ran_out() -> bool {
    PROGRAM_COPIES_RAN_OUT.with(std::cell::Cell::get)
}

/// Whether a copy of the standard library's text found no room this round, below the copies made
/// so far and above where the library's own text ends. The round refuses the program then too.
pub(crate) fn std_copies_ran_out() -> bool {
    STD_COPIES_RAN_OUT.with(std::cell::Cell::get)
}

/// Reserves `length` spans in a row for a copy of the standard library's text, below every copy
/// made so far, giving the lowest. Nothing, when they've run out.
fn std_copies_reserved(length: u32) -> Option<u32> {
    COPIES.with(|copies| {
        let Some(base) = copies.get().checked_sub(length).filter(|base| *base >= COPIES_FROM) else {
            STD_COPIES_RAN_OUT.with(|ran_out| ran_out.set(true));
            return None;
        };
        copies.set(base);
        Some(base)
    })
}

/// Gives `module` the copies written in so far, in order of where each went.
pub(crate) fn copies_into(module: &mut Module) {
    let mut copied = COPIED.with(|copied| std::mem::take(&mut *copied.borrow_mut()));
    copied.sort_by_key(|copy| copy.at);
    module.copied = copied;
}

/// The program's own text a copy keeps together: spans no further apart than this are one
/// stretch, moved as one, so a statement's spans keep where each is against the next.
const STRETCH_GAP: u32 = 256;

/// What a span in a copy is, for how it moves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The standard library's, which `moved_apart` moves.
    Std,
    /// The program's own text.
    Own,
    /// In the earlier copy at this place in `COPIED`.
    Copy(usize),
    /// Something lowering wrote in, which points at no text.
    Written,
}

/// Gives a copy of some statements written in spans of its own, so what's found by span in it, as
/// a type the checker found there, is never another copy's. The program's own text moves in
/// stretches, each keeping where its parts are against each other, as does each earlier copy
/// inside it, and each is noted with the text it copies (`Module::copied`), so what says where
/// something is still points at the program's text. Each span lowering wrote in gets a new one. The
/// standard library's are `moved_apart`'s. Nothing, when the spans lowering writes in have run out.
pub(crate) fn copied_apart(region: Region, parts: &mut [&mut [Stmt]]) -> Option<()> {
    copied_apart_by(region, &mut |moved| {
        for part in parts.iter_mut() {
            every_span(part, moved);
        }
    })
}

/// A copy of a function's parameters and body on spans of its own, as `copied_apart` gives them.
/// The function's own span stays, which says whose it is.
pub(crate) fn copied_def_apart(region: Region, def: &mut FnDef) -> Option<()> {
    copied_apart_by(region, &mut |moved| {
        for param in &mut def.params {
            param.span = moved(param.span);
            if let Some(ty) = &mut param.ty {
                type_spans(ty, moved);
            }
        }
        for ty in def.ret.iter_mut().chain(def.fails.as_mut()) {
            type_spans(ty, moved);
        }
        every_span(&mut def.body, moved);
    })
}

/// What changes every span of something through what it's handed.
type Spans<'s> = dyn FnMut(&mut dyn FnMut(Span) -> Span) + 's;

/// `copied_apart` for whatever `spans` changes every span of, through what it's handed: it's
/// handed something that leaves each as it is first, then what moves each.
fn copied_apart_by(region: Region, spans: &mut Spans<'_>) -> Option<()> {
    let made = COPIED.with(|copied| copied_apart_from(region, spans, &copied.borrow()));
    // Each kept where it goes among the others, in order of where it went.
    let pending = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    COPIED.with(|copied| {
        let mut copied = copied.borrow_mut();
        for copy in pending {
            let at = copied.partition_point(|known| known.at < copy.at);
            copied.insert(at, copy);
        }
    });
    made
}

/// `copied_apart_by`, with the copies made so far, in order of where each went.
fn copied_apart_from(region: Region, spans: &mut Spans<'_>, copied: &[Copied]) -> Option<()> {
    let kind = |span: Span| {
        if span.in_prelude() {
            return Kind::Std;
        }
        let after = copied.partition_point(|copy| copy.at <= span.start);
        match after.checked_sub(1).and_then(|at| copied.get(at).map(|copy| (at, copy))) {
            Some((at, copy)) if span.start - copy.at < copy.len => Kind::Copy(at),
            _ if written_in(span) => Kind::Written,
            _ => Kind::Own,
        }
    };
    let mut own: Vec<(u32, u32)> = Vec::new();
    let mut earlier: BTreeSet<usize> = BTreeSet::new();
    spans(&mut |span| {
        match kind(span) {
            Kind::Own => own.push((span.start, span.end.max(span.start))),
            Kind::Copy(at) => {
                earlier.insert(at);
            }
            Kind::Std | Kind::Written => {}
        }
        span
    });
    let Moved { made, own: moved_own, earlier: moved_earlier } =
        moved(region, &stretched(own), &earlier, copied)?;
    let shifted = |span: Span, from: u32, to: u32| Span {
        start: span.start - from + to,
        end: span.end.max(span.start) - from + to,
    };
    let mut written: BTreeMap<(u32, u32), Span> = BTreeMap::new();
    let mut moves: Vec<Move> = Vec::new();
    spans(&mut |span| {
        let to = match kind(span) {
            Kind::Own => {
                let after = moved_own.partition_point(|(start, ..)| *start <= span.start);
                after
                    .checked_sub(1)
                    .and_then(|at| moved_own.get(at))
                    .map_or(span, |&(start, _, at)| shifted(span, start, at))
            }
            Kind::Copy(index) => match (copied.get(index), moved_earlier.get(&index)) {
                (Some(copy), Some(&at)) => shifted(span, copy.at, at),
                _ => span,
            },
            Kind::Written => *written.entry((span.start, span.end)).or_insert_with(|| {
                let at = reserved(0);
                Span { start: at, end: at }
            }),
            Kind::Std => span,
        };
        if to != span {
            moves.push(((span.start, span.end), (to.start, to.end)));
        }
        to
    });
    MOVED.with(|moved| moved.borrow_mut().extend(moves));
    PENDING.with(|pending| pending.borrow_mut().extend(made));
    Some(())
}

/// The program's text a copy holds, as each of its spans' first and last, in stretches, each from
/// its first span to its last.
fn stretched(mut own: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    own.sort_unstable();
    let mut stretches: Vec<(u32, u32)> = Vec::new();
    for (start, end) in own {
        match stretches.last_mut() {
            Some((_, last)) if start <= last.saturating_add(STRETCH_GAP) => *last = (*last).max(end),
            _ => stretches.push((start, end)),
        }
    }
    stretches
}

/// Where what a copy holds goes: each copy made, noted with the text it copies; each stretch of
/// the program's text, as its first span, its last, and where it goes; and where each earlier copy
/// inside it goes, by its place in the copies so far.
struct Moved {
    made: Vec<Copied>,
    own: Vec<(u32, u32, u32)>,
    earlier: BTreeMap<usize, u32>,
}

/// Where `stretches` of the program's text and the `earlier` copies inside a copy go, in `region`.
/// They're moved in one run, in the order they're in now, so what's before something else still
/// is. Nothing, when spans have run out.
fn moved(
    region: Region,
    stretches: &[(u32, u32)],
    earlier: &BTreeSet<usize>,
    copied: &[Copied],
) -> Option<Moved> {
    // Each as its first span, its last, and the text its first span copies.
    let mut moving: Vec<(u32, u32, u32, Option<usize>)> =
        stretches.iter().map(|&(start, end)| (start, end, start, None)).collect();
    for &index in earlier {
        let Some(copy) = copied.get(index) else { continue };
        moving.push((copy.at, copy.at + copy.len.saturating_sub(1), copy.from, Some(index)));
    }
    moving.sort_unstable();
    let needed: u64 = moving.iter().map(|(start, end, ..)| u64::from(end - start) + 1).sum();
    if region == Region::Written {
        let left = NEXT.with(std::cell::Cell::get).saturating_sub(WRITTEN_FROM);
        if needed >= u64::from(left) {
            return None;
        }
    }
    let mut next = match needed.checked_sub(1) {
        Some(last) => reserved_in(region, u32::try_from(last).ok()?)?,
        None => 0,
    };
    let mut laid = Moved { made: Vec::new(), own: Vec::new(), earlier: BTreeMap::new() };
    for (start, end, from, earlier) in moving {
        laid.made.push(Copied { at: next, len: end - start + 1, from });
        match earlier {
            Some(index) => {
                laid.earlier.insert(index, next);
            }
            None => laid.own.push((start, end, next)),
        }
        next += end - start + 1;
    }
    Some(laid)
}

/// Changes every span in some statements through `moved`, the types they write among them.
pub(crate) fn every_span(body: &mut [Stmt], moved: &mut dyn FnMut(Span) -> Span) {
    fennec_syntax::visit::written_types_in(body, &mut |ty| type_spans(ty, moved));
    fennec_syntax::visit::respan_stmts(body, moved);
}

/// Changes the span of a written type, and of each type in it, through `moved`.
fn type_spans(ty: &mut TypeExpr, moved: &mut dyn FnMut(Span) -> Span) {
    ty.span = moved(ty.span);
    for arg in &mut ty.args {
        type_spans(arg, moved);
    }
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
    let Some(base) = std_copies_reserved(high - low + 1) else { return };
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
    NEXT.with(|next| next.set(next.get().min(below_copies(written_low) - 1)));
}

/// `low`, or the first span of a copy among what lowering writes that's lower, which takes spans
/// no expression in it may: a statement's, or those between the stretches it keeps together.
fn below_copies(low: u32) -> u32 {
    COPIED.with(|copied| {
        copied.borrow().iter().map(|copy| copy.at).filter(|at| *at >= WRITTEN_FROM).fold(low, u32::min)
    })
}

/// Starts the spans lowering writes, and where copies of the standard library's text go, where a
/// new thread starts them, as a new process does: what a program's build writes in is the same
/// however many programs were built on its thread before it.
pub(crate) fn start_program() {
    NEXT.with(|next| next.set(Span::PRELUDE - 1));
    COPIES.with(|copies| copies.set(u32::MAX));
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
const WRITTEN_FROM: u32 = Span::WRITTEN;

/// Whether lowering wrote what's at `span` in, rather than a program or the standard library.
pub(crate) fn written_in(span: Span) -> bool {
    (WRITTEN_FROM..Span::PRELUDE).contains(&span.start)
}

/// Whether what's at `span` is anything but a program's own text: the standard library's, or what
/// lowering wrote in, from text parsed as std's is or not.
pub(crate) fn not_written_by_hand(span: Span) -> bool {
    span.start >= WRITTEN_FROM
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
        // A source runs only where it's written in, from its body as it was checked, and what's
        // in that body is written in there, around the caller's block. Written in here too, a
        // `with` around its `lend` would put the `lend` in the loop a failing block runs in.
        if def.ret.as_ref().is_some_and(|ret| ret.name == crate::lend::WITH) {
            return;
        }
        self.returns.push(def.ret.clone());
        self.failing.push(def.fails.is_some());
        self.fails.push(def.fails.clone());
        let params: Vec<String> = def.params.iter().map(|param| param.name.clone()).collect();
        self.binds.push(fennec_syntax::sight::bound_in(&def.body, &params));
        if def.ret.is_some() {
            self.tail_returned(&mut def.body);
            // A source and a generator give back no last value, so a `with` ending one needs no
            // ending that says it always leaves.
            let source = def.ret.as_ref().is_some_and(|ret| ret.name == crate::lend::WITH);
            if !source && !fennec_syntax::fuse::has_yield(&def.body) {
                tail_withs(&def.body, &mut self.tails);
            }
        }
        self.stmts(&mut def.body);
        self.binds.pop();
        self.fails.pop();
        self.failing.pop();
        self.returns.pop();
    }

    /// Readies a block's failures for its cleanup. With somewhere to hold one, each `?` becomes a
    /// `match` that fails, and each failure is to leave the loop the block runs in, to the cleanup
    /// written once after it. Otherwise the cleanup is written in before each. Says which.
    fn ready_failures(&self, block: &mut Vec<Stmt>, cleanup: &[Stmt]) -> bool {
        let funneled = !cleanup.is_empty() && self.held_failure().is_some();
        if funneled {
            fail_through(block, &[]);
        } else if !cleanup.is_empty() {
            fail_through(block, cleanup);
        }
        funneled
    }

    /// The names a block's ways out are kept in, set before it: which way it left, what it
    /// returns, and what it fails with.
    fn ways_kept(
        &self,
        ways: Ways,
        (exit, returned, failed): (&str, &str, &str),
        at: Span,
    ) -> Option<Vec<Stmt>> {
        let mut kept = Vec::new();
        // A failure is held in `failed` alone, so `exit` is only for the other ways out.
        if ways.returned.is_some() || ways.broke || ways.continued {
            kept.push(assign(exit, int(0, at), true, None));
        }
        if ways.returned == Some(true) {
            let mut optional = self.returns.last().cloned().flatten()?;
            optional.optional = true;
            kept.push(assign(returned, Expr { kind: ExprKind::None, span: here(at) }, true, Some(optional)));
        }
        if ways.failed {
            let held = self.held_failure()?;
            kept.push(assign(failed, Expr { kind: ExprKind::None, span: here(at) }, true, Some(held)));
        }
        Some(kept)
    }

    /// The type a failure leaving a block is held in, until its cleanup has run: what the
    /// function it's in fails with, or `None`.
    fn held_failure(&self) -> Option<TypeExpr> {
        if self.failing.last() != Some(&true) {
            return None;
        }
        let mut held = self.fails.last().cloned().flatten()?;
        held.optional = true;
        Some(held)
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
            ExprKind::Else { value: inner, default, .. } if default.leaves() => inner.as_ref(),
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
        let mut cleanup = failing_with(name(&held, at), cleanup, at);
        // Each way the cleanup fails leaves it at one `fail`, where the `with` around it, if any,
        // cleans up after it: it's written once, however many ways this one fails.
        if let Some(failure) = self.held_failure() {
            cleanup = funneled(cleanup, failure, at);
        }
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
                        ExprKind::Else { value: inner, default, .. } if default.leaves() => {
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
            StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
                self.stmts(body);
            }
            StmtKind::OnFailure { cleanup, body, .. } => {
                self.stmts(cleanup);
                self.stmts(body);
            }
            StmtKind::Lend { value, otherwise, .. } => {
                self.cases_in(value);
                self.stmts(otherwise);
            }
            StmtKind::Def(def) => self.function(def),
            StmtKind::Expr(value)
            | StmtKind::Assign { value, .. }
            | StmtKind::Return(Some(value))
            | StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value) => self.cases_in(value),
            _ => {}
        }
    }

    /// The blocks inside an expression, written in first: each `match`'s cases, wherever it is,
    /// as the blocks of an `except` or `else` are. Only the outermost `match`es' cases are walked
    /// here, since walking a case reaches each `match` inside it, so each block is walked once.
    fn cases_in(&mut self, value: &mut Expr) {
        outer_cases(value, &mut |cases| self.stmts(cases));
    }

    /// A `match` with `ref` cases, each written as a case that binds the field as a plain name
    /// and puts the case back, with that name's value, wherever its body ends: when it finishes,
    /// leaves by `return`, `break` or `continue`, or fails.
    fn ref_match(&mut self, value: &Expr, at: Span) -> Option<Vec<Stmt>> {
        let ExprKind::Match { subject, arms } = &value.kind else { return None };
        let cases = self.ref_matches.get(&(value.span.start, value.span.end))?;
        // Written in once: what's written in has no `ref` left in it.
        if !arms.iter().any(|arm| arm.pattern.holds_ref()) {
            return None;
        }
        // Every case is ready to be written in before anything is: a `match` with one that can't
        // be is left whole, which lowering then refuses, rather than this pass saying it wrote
        // something, round after round.
        let ready = cases.len() == arms.len()
            && arms.iter().zip(cases).all(|(arm, fields)| match fields {
                Some(fields) => rewritable(&arm.pattern, fields),
                None => !arm.pattern.holds_ref(),
            });
        if !ready {
            return None;
        }
        // The place the `match` is over, each index along it held in a name first: the case is
        // put back where it was matched, whatever its body does to those names, as a `ref` name
        // keeps the place it was given.
        self.made += 1;
        let made = self.made;
        let mut written_in = Vec::new();
        let subject = crate::ref_lowering::indexes_held(
            subject,
            &|count| format!("place{count}__ref{made}"),
            &mut written_in,
        );
        let mut written = arms.clone();
        for (arm, fields) in written.iter_mut().zip(cases) {
            let Some(fields) = fields else { continue };
            self.ref_case(arm, &subject, fields, at)?;
        }
        let kind = ExprKind::Match { subject: Box::new(subject), arms: written };
        written_in.push(stmt(StmtKind::Expr(Expr { kind, span: value.span }), at));
        Some(written_in)
    }

    /// One `ref` case, written in: each `ref` bound as a name the body may change, each `_` bound
    /// to a hidden name, and the case put back, made from them, on every way out. Its guard and
    /// body are left as they're written, so each name in them means what it did. Each part is
    /// taken in its field's place, whether it's written by position or by name, and a field it
    /// leaves out is a `_`. The place it's put back into holds its indexes in names of their own
    /// (`ref_match`), so a name the case binds, as `at` in
    /// `match widgets[at].kind: case .Slider(ref at, most):`, isn't one it reads.
    fn ref_case(&mut self, arm: &mut MatchArm, subject: &Expr, fields: &[String], at: Span) -> Option<()> {
        let Pattern::Variant { name: case, args, fields: named, .. } = &mut arm.pattern else { return None };
        *args = in_fields_order(std::mem::take(args), named, fields)?;
        *named = fields.to_vec();
        self.made += 1;
        let made = self.made;
        let parts = parts_bound(args, fields, made, at)?;
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
        // A case whose body always returns returns after the case is put back, every time, so
        // what's written in ends the function as the body did.
        let always = always_returns(&block);
        // Each failure leaves the loop the case runs in, and the case is put back once after it,
        // as for a `with`'s block.
        let funneled = self.ready_failures(&mut block, std::slice::from_ref(&put_back));
        let (exit, returned) = (format!("exit__ref{made}"), format!("returned__ref{made}"));
        let failed = format!("failed__ref{made}");
        let mut caller = Caller::new(&exit, &returned, at);
        caller.failed = funneled.then_some(failed.as_str());
        exits(&mut block, &mut caller, false);
        let ways = caller.ways;
        let mut block = self.on_failure(std::slice::from_ref(&put_back), block, at);
        let mut body = self.ways_kept(ways, (&exit, &returned, &failed), at)?;
        if ways.any() {
            block.push(stmt(StmtKind::Break, at));
            let always = Expr { kind: ExprKind::Bool(true), span: here(at) };
            body.push(stmt(StmtKind::While { cond: always, body: block }, at));
        } else {
            body.extend(block);
        }
        if ways.failed {
            body.push(failing_if_held(&failed, std::slice::from_ref(&put_back), at));
        }
        body.push(put_back);
        if always && ways.returned == Some(true) {
            body.extend(returned_always(&returned, at));
        } else if always && ways.failed && ways.returned.is_none() && !ways.broke && !ways.continued {
            // A matched case's body always runs, so one that always fails always leaves by
            // failing, through the put-back.
            body.extend(failed_always(&failed, at));
        } else {
            body.extend(taken_again(ways, &exit, &returned, at));
        }
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
        let (mut written, renamed) = bound(def, callee, args, source.method, &hidden)?;
        let mut body = def.body.clone();
        hidden_in_source(&mut body, &renamed, &hidden, &self.binds);
        if let Some(task) = &source.task {
            task.written_in(&mut body, &hidden)?;
        }
        if let Some(instead) = self.instead.take() {
            fail_instead(&mut body, &instead);
        }
        typed_for_call(source, &mut written, &mut body)?;
        // Types are found by span, and each call gives the body types of its own, as `None` for a
        // parameter one call leaves out and another gives, or a member of a union a parameter is
        // narrowed to, so each written-in copy of a source's text gets spans of its own: the
        // standard library's and the program's (#2056).
        moved_apart(&mut written, &mut body);
        copied_apart(Region::Program, &mut [&mut written, &mut body])?;
        // The caller's name for what's lent: the source's own name for it, when it lends one,
        // so the block's changes are what the source's cleanup sees, or else a name of its own,
        // apart from the source's hidden ones.
        let lent_as = format!("{}__lent{}", held.name, self.made);
        let exit = hidden("exit");
        let returned = hidden("returned");
        let failed = hidden("failed");
        let mut caller = Caller::new(&exit, &returned, at);
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
            if self.held_failure().is_none() {
                cleanup_reads_kept(&mut block, &unmarked, at);
            }
            let funneled = self.ready_failures(&mut block, &unmarked);
            caller.ways = Ways::default();
            caller.failed = funneled.then_some(failed.as_str());
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
            if caller.ways.failed {
                lent.push(failing_if_held(&failed, otherwise, at));
            }
            Some(lent)
        }) {
            return None;
        }
        // The block's way out, kept in names set before the source's body, so they're in sight
        // after it, and taken again once its cleanup has run.
        let ways = caller.ways;
        written.extend(self.ways_kept(ways, (&exit, &returned, &failed), at)?);
        written.extend(body);
        written.extend(self.ending((always_returns, ways), (&exit, &returned, &failed), at));
        Some(written)
    }

    /// What's written after a source's body: the block's way out taken again. A block that always
    /// returns gives back what it returned, which says to the check that the body always leaves.
    /// So does one that always fails, where the `with` is a value-giving function's last line, the
    /// one place the check needs it. Nothing reaches it, as a source reaches its `lend` on every way
    /// that doesn't fail (#2066).
    fn ending(
        &self,
        (always_returns, ways): (bool, Ways),
        (exit, returned, failed): (&str, &str, &str),
        at: Span,
    ) -> Vec<Stmt> {
        let only = !ways.broke && !ways.continued;
        if always_returns && ways.returned == Some(true) && only {
            returned_always(returned, at)
        } else if always_returns
            && ways.failed
            && ways.returned.is_none()
            && only
            && self.tails.contains(&(at.start, at.end))
        {
            failed_always(failed, at)
        } else {
            taken_again(ways, exit, returned, at)
        }
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

/// A type as written where a type goes and as written in brackets, built straight from it, never
/// from its text, so a name the checker holds that a program couldn't write, as `Delivery'` for a
/// program's own `Delivery`, comes through as it is. It has spans of its own, which no program's
/// text reaches. `None` for a type with no written form, nothing and several errors among them.
pub(crate) fn written_type(ty: &crate::types::Type) -> Option<(TypeExpr, Expr)> {
    if matches!(ty, crate::types::Type::Unit | crate::types::Type::Errors(_)) {
        return None;
    }
    // Two spans of its own: a tuple of types in brackets starts one after its first item, as
    // `TypeExpr::as_expr` writes it, so it reads back as one type, not as the brackets' several.
    here(Span::default());
    let low = here(Span::default()).start;
    let span = Span { start: low, end: low + 1 };
    let written = ty.written(span)?;
    let bracketed = written.as_expr(span)?;
    Some((written, bracketed))
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

/// A source's body, written in at a call with its parameters' uses as `renamed` says and every
/// name it binds where the caller's block could be in its sight hidden, so none is one of the
/// caller's: each declaration, loop, `with`, case, cleanup and nested function. Each is renamed
/// with only the uses in its sight, so a name the body reads that means something else, as a
/// function, keeps its meaning. An arrow function's, a comprehension's and a nested function's
/// parameters can't have the block in their sight and keep their names, unless they're a name of
/// the caller's a parameter's uses are written as, which they'd take for their own.
///
/// A name the body reads from outside, a function or constant of the program's, that the caller
/// binds anywhere (`binds`) is written as meaning the source's (`sight::as_outer`), so nothing in
/// sight where it's written in takes it.
fn hidden_in_source(
    body: &mut [Stmt],
    renamed: &[(String, String)],
    hidden: &dyn Fn(&str) -> String,
    binds: &[BTreeSet<String>],
) {
    let outside: Vec<(String, Option<String>)> =
        renamed.iter().map(|(param, written)| (param.clone(), Some(written.clone()))).collect();
    let callers: Vec<&String> =
        renamed.iter().map(|(_, written)| written).filter(|written| !written.contains("__")).collect();
    // A function or constant the source names, which something the caller binds would name there.
    let taken = |name: &str| binds.iter().any(|bound| bound.contains(name));
    fennec_syntax::sight::hide_bound_and_free(
        body,
        &outside,
        &|binder, name| match binder {
            Binder::Parameter | Binder::Arrow | Binder::Comprehension => {
                callers.iter().any(|caller| *caller == name).then(|| hidden(name))
            }
            _ => Some(hidden(name)),
        },
        &|name, _| taken(name).then(|| fennec_syntax::sight::as_outer(name)),
    );
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

/// The spans of the `with`s that are `body`'s last line, through an `if` with an `else`, a
/// `match`'s cases, and a `with`'s own block.
fn tail_withs(body: &[Stmt], tails: &mut BTreeSet<(u32, u32)>) {
    let Some(last) = body.last() else { return };
    match &last.kind {
        StmtKind::With { body: block, lock: false, .. } => {
            tails.insert((last.span.start, last.span.end));
            tail_withs(block, tails);
        }
        StmtKind::If { branches, otherwise: Some(otherwise) } => {
            for (_, branch) in branches {
                tail_withs(branch, tails);
            }
            tail_withs(otherwise, tails);
        }
        StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
            for arm in arms {
                tail_withs(&arm.body, tails);
            }
        }
        _ => {}
    }
}

/// What's written after a source's body whose block always fails: `assert failed is not None`,
/// which narrows it, and `fail failed`. The failure left where the block did, through the source's
/// cleanup, so nothing reaches this; it says to the check that the body always leaves, as the
/// block did.
fn failed_always(failed: &str, at: Span) -> Vec<Stmt> {
    vec![stmt(StmtKind::Assert(is_held(failed, at)), at), stmt(StmtKind::Fail(name(failed, at)), at)]
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
    /// Whether it fails, held in the caller's `failed`.
    failed: bool,
}

impl Ways {
    fn any(self) -> bool {
        self.returned.is_some() || self.broke || self.continued || self.failed
    }
}

/// What binding a source's parameters writes first, and the names they're renamed to.
type Bound = (Vec<Stmt>, Vec<(String, String)>);

/// What a `lend`'s value and `else:` block are written in as.
type LentAs<'a> = dyn FnMut(Expr, &[Stmt]) -> Option<Vec<Stmt>> + 'a;

struct Caller<'n> {
    exit: &'n str,
    returned: &'n str,
    /// Where a failure is held as it leaves the block, when its cleanup runs once after it.
    failed: Option<&'n str>,
    /// How many `fail`s have been written as leaving, to tell which loops hold one.
    fails_made: usize,
    ways: Ways,
    span: Span,
}

impl<'n> Caller<'n> {
    fn new(exit: &'n str, returned: &'n str, span: Span) -> Self {
        Caller { exit, returned, failed: None, fails_made: 0, ways: Ways::default(), span }
    }
}

/// Replaces the `lend` in a body with what `with_block` makes of its value and `else:` block,
/// saying whether there was one.
fn replace_lend(body: &mut Vec<Stmt>, with_block: &mut LentAs<'_>) -> bool {
    for at in 0..body.len() {
        let Some(stmt) = body.get_mut(at) else { return false };
        if let StmtKind::Lend { value, otherwise, .. } = &stmt.kind {
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

/// Whether a failure inside this statement's body leaves as a failure, not held by the block and
/// leaving the loop it runs in as a `break` would:
/// - a `parallel` round's leaves where the loop ends, which isn't the block's to write;
/// - a loop over a generator that can fail, `for x in made()?:` or `for x in made()? from Wrap(...)`,
///   lets go of it as a failure leaves, noting a failure of the generator's own cleanup on that one;
/// - a `lock` rolls back what its body changed as a failure leaves it.
fn fails_unheld(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::For { iter, .. } => {
            crate::parallel::parallel_items(iter).is_some()
                || matches!(iter.kind, ExprKind::Try { .. })
                || owns_inside(iter, true)
        }
        // As it's written, or as `lock_lowering` wrote it in an earlier round, as a `ref` case in a
        // source's body is lowered once the source is written in.
        StmtKind::With { lock, names, .. } => *lock || crate::lock::lowered(names),
        _ => false,
    }
}

/// Whether a loop goes over `enumerate(...)` or `zip(...)` with a `?` that's a whole argument, whose
/// loop owns the generator that gives, as it does one it goes over by itself: one with `from` too
/// when `wrapped` says.
fn owns_inside(iter: &Expr, wrapped: bool) -> bool {
    let whole_try = |arg: &fennec_syntax::Arg| match &arg.value.kind {
        ExprKind::Try { context, .. } => wrapped || context.is_none(),
        _ => false,
    };
    matches!(&iter.kind, ExprKind::Call { callee, args }
        if matches!(&callee.kind, ExprKind::Name(name) if name == "enumerate" || name == "zip")
            && args.iter().any(whole_try))
}

/// `exits` in a body inside the block, where the block's failure isn't held when `unheld` says.
fn exits_within(body: &mut Vec<Stmt>, caller: &mut Caller<'_>, in_loop: bool, unheld: bool) {
    let failed = if unheld { caller.failed.take() } else { None };
    exits(body, caller, in_loop);
    if unheld {
        caller.failed = failed;
    }
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
        let unheld = fails_unheld(stmt);
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
            StmtKind::Fail(failure) if let Some(failed) = caller.failed => {
                caller.ways.failed = true;
                caller.fails_made += 1;
                Some(vec![assign(failed, failure.clone(), false, None), stmt_break(span)])
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
                let fails_before = caller.fails_made;
                exits_within(body, caller, true, unheld);
                // A `return` or a failure inside it leaves this loop too.
                let mut leaves = Vec::new();
                if caller.ways.returned.is_some() && !before {
                    leaves.push(dispatch(exit, RETURNED, StmtKind::Break, span));
                }
                if caller.fails_made > fails_before
                    && let Some(failed) = caller.failed
                {
                    leaves.push(leave_if_held(failed, span));
                }
                (!leaves.is_empty()).then(|| std::iter::once(stmt.clone()).chain(leaves).collect())
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
                exits_within(body, caller, in_loop, unheld);
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
        // A `lock` `lock_lowering` wrote in is never written in again: it waits on nothing.
        StmtKind::With { lock: false, names, .. } if !crate::lock::lowered(names) => true,
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
/// The `match`es are told apart by where they are, worked out before `each` changes anything, so
/// one `each` adds isn't walked. Spans won't do: a pass that writes code in can give two `match`es
/// one span, as the cases under `except why:` once had. Only cases are changed in between, and no
/// outer `match` is inside one, so each stays where it was until it's reached, and nothing moved
/// can be where it is.
fn outer_cases(value: &mut Expr, each: &mut dyn FnMut(&mut Vec<Stmt>)) {
    let place = |expr: &Expr| std::ptr::from_ref(expr).addr();
    let mut inner = Vec::new();
    let mut outer = Vec::new();
    // Only read here, though the walk takes it to change.
    fennec_syntax::visit::in_expr(value, &mut |expr| {
        let at = place(expr);
        match &mut expr.kind {
            ExprKind::Match { arms, .. } => {
                outer.push(at);
                for arm in arms {
                    fennec_syntax::visit::in_stmts(&mut arm.body, &mut |deeper| {
                        if matches!(deeper.kind, ExprKind::Match { .. }) {
                            inner.push(place(deeper));
                        }
                    });
                }
            }
            ExprKind::Lambda { body, .. } => {
                fennec_syntax::visit::in_expr(body, &mut |deeper| {
                    if matches!(deeper.kind, ExprKind::Match { .. }) {
                        inner.push(place(deeper));
                    }
                });
            }
            _ => {}
        }
    });
    outer.retain(|at| !inner.contains(at));
    if outer.is_empty() {
        return;
    }
    fennec_syntax::visit::in_expr(value, &mut |expr| {
        let at = place(expr);
        if let ExprKind::Match { arms, .. } = &mut expr.kind
            && let Some(found) = outer.iter().position(|outside| *outside == at)
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
                before.extend(cleanup_copy(otherwise));
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
        // The `?`s inside what it's of run first, each with the cleanup on its failure.
        let mut value = value.as_ref().clone();
        hoist_inside(&mut value, otherwise, before, made, span);
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
            kind: ExprKind::Match { subject: Box::new(value), arms: vec![ok, failed_arm(otherwise, span)] },
            span: here(span),
        };
        held.kind = StmtKind::Expr(matched);
        return true;
    }
    let span = held.span;
    match &mut held.kind {
        // A loop over a generator that can fail, `for x in made()?:`, owns the generator: it lets
        // go of it when it's left, before the cleanup runs, with a failure of the generator's own
        // cleanup noted on the one leaving. Where the block holds its failure (`otherwise` is
        // empty), it has an `OnFailure` its failures reach as any from inside a call do, so only
        // the `?`s inside what it's of come out, and the loop's stays. Where the cleanup is written
        // in before each failure, as in a `test`, nothing else would run it on the call's failure.
        StmtKind::For { iter: Expr { kind: ExprKind::Try { value: inner, context: None }, .. }, .. }
            if otherwise.is_empty() =>
        {
            hoist_inside(inner, otherwise, before, made, span);
        }
        // So does one over `enumerate(made()?)` or `zip(made()?, ...)`, for the generator made
        // right there: its `?` stays too, where that keeps what runs in the order it's written.
        StmtKind::For { iter: call @ Expr { kind: ExprKind::Call { .. }, .. }, .. }
            if otherwise.is_empty() && owns_inside(call, false) =>
        {
            hoist_owning(call, otherwise, before, made, span);
        }
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value))
        | StmtKind::For { iter: value, .. }
        | StmtKind::With { value, .. } => hoist_inside(value, otherwise, before, made, span),
        StmtKind::Assign { target, value, .. } => {
            hoist_inside(value, otherwise, before, made, span);
            hoist_inside(target, otherwise, before, made, span);
        }
        StmtKind::If { branches, .. } => {
            if let Some((cond, _)) = branches.first_mut() {
                hoist_inside(cond, otherwise, before, made, span);
            }
        }
        _ => {}
    }
    // The line stays, its `?`s taken out before it, for the rest of the walk to look at.
    false
}

/// Takes the `?`s out of `value` into `before` in the order they run, each with the cleanup on its
/// failure, leaving a name for what each gave. Anything that runs before a `?`, to its left, goes
/// into a name of its own first, so it still runs first, as `f(log(x), g()?)` runs `log` before
/// `g`. What runs after the last `?` stays where it is.
fn hoist_inside(value: &mut Expr, otherwise: &[Stmt], before: &mut Vec<Stmt>, made: &mut usize, span: Span) {
    let holding: Vec<bool> = runs_first(value).into_iter().map(holds_try).collect();
    let places = places_first(value);
    if let Some(last) = holding.iter().rposition(|holds| *holds) {
        let parts = runs_first(value).into_iter().zip(holding).zip(places);
        for ((part, holds), place) in parts.take(last + 1) {
            if holds {
                hoist_inside(part, otherwise, before, made, span);
            } else if !place && !written_out(part) {
                let kept = format!("ran__try{}_{}", here(span).start, *made);
                *made += 1;
                // The name keeps the span of what it stands for, which its type is found by.
                let held = Expr { kind: ExprKind::Name(kept.clone()), span: part.span };
                let ran = std::mem::replace(part, held);
                before.push(assign(&kept, ran, false, None));
            }
        }
    }
    // A `?` itself, once what it's of has run: a name for what it gives, or the cleanup on its
    // failure. A span of its own, as each written-in copy of a source gets the same `?` at the same
    // place, and two copies in one block would declare one name twice.
    if let ExprKind::Try { value: inner, context: None } = &mut value.kind {
        let got = format!("got__try{}_{}", here(span).start, *made);
        *made += 1;
        let inner = std::mem::replace(inner.as_mut(), name(&got, span));
        let at = value.span;
        let ok = arm("Ok", "value__try", vec![stmt_value(name("value__try", span))], span);
        let matched = Expr {
            kind: ExprKind::Match { subject: Box::new(inner), arms: vec![ok, failed_arm(otherwise, span)] },
            span: here(span),
        };
        before.push(assign(&got, matched, false, None));
        // As the `?` it stands for, whose span its type is found by.
        *value = Expr { kind: ExprKind::Name(got), span: at };
    }
}

/// `hoist_inside` for `enumerate(...)` or `zip(...)` a loop goes over, keeping each `?` that's a
/// whole argument where it is when nothing after it has to run first, so the loop owns the
/// generator it gives. Each part before the last that has to run first is named first, in order,
/// as `hoist_inside` does, a kept `?` among them only when what it's of holds one.
fn hoist_owning(call: &mut Expr, otherwise: &[Stmt], before: &mut Vec<Stmt>, made: &mut usize, span: Span) {
    let whole_try = |part: &Expr| matches!(part.kind, ExprKind::Try { context: None, .. });
    let inner_holds = |part: &mut Expr| match &mut part.kind {
        ExprKind::Try { value, context: None } => holds_try(value),
        _ => false,
    };
    // Which whole-argument `?`s stay: each with nothing after it that has to run first.
    let mut kept = Vec::new();
    let mut later = false;
    for part in runs_first(call).into_iter().rev() {
        let keep = !later && whole_try(part);
        later |= if keep { inner_holds(part) } else { holds_try(part) };
        kept.push(keep);
    }
    kept.reverse();
    let places = places_first(call);
    let holding: Vec<bool> = runs_first(call)
        .into_iter()
        .zip(&kept)
        .map(|(part, keep)| if *keep { inner_holds(part) } else { holds_try(part) })
        .collect();
    let Some(last) = holding.iter().rposition(|holds| *holds) else { return };
    let parts = runs_first(call).into_iter().zip(holding).zip(kept).zip(places);
    for (((part, holds), keep), place) in parts.take(last + 1) {
        if keep {
            if let ExprKind::Try { value: inner, context: None } = &mut part.kind {
                hoist_inside(inner, otherwise, before, made, span);
            }
        } else if holds {
            hoist_inside(part, otherwise, before, made, span);
        } else if !place && !written_out(part) {
            let held = format!("ran__try{}_{}", here(span).start, *made);
            *made += 1;
            // The name keeps the span of what it stands for, which its type is found by.
            let name_of = Expr { kind: ExprKind::Name(held.clone()), span: part.span };
            let ran = std::mem::replace(part, name_of);
            before.push(assign(&held, ran, false, None));
        }
    }
}

/// What in `value` always runs, before it does, in the order it runs: a call's receiver and
/// arguments, an operator's sides, a list's items, and so on. A `?` that might not run, in a
/// branch, after `else` or past `and` or `or`, is refused in a source's block
/// (`lend::unplaced_try`), so what might not run isn't here.
fn runs_first(value: &mut Expr) -> Vec<&mut Expr> {
    match &mut value.kind {
        ExprKind::Call { callee, args } => {
            let mut parts = Vec::with_capacity(args.len() + 1);
            parts.extend(runs_before_call(callee));
            parts.extend(args.iter_mut().map(|arg| &mut arg.value));
            parts
        }
        ExprKind::Attr { object, .. } => vec![object.as_mut()],
        ExprKind::Index { object, index } => vec![object.as_mut(), index.as_mut()],
        ExprKind::Binary { op: fennec_syntax::BinOp::And | fennec_syntax::BinOp::Or, lhs, .. } => {
            vec![lhs.as_mut()]
        }
        ExprKind::Binary { lhs, rhs, .. } => vec![lhs.as_mut(), rhs.as_mut()],
        ExprKind::FStr(parts) => parts
            .iter_mut()
            .filter_map(|part| match part {
                fennec_syntax::FStrPart::Expr { expr, .. } => Some(expr),
                fennec_syntax::FStrPart::Text(_) => None,
            })
            .collect(),
        ExprKind::Dict(pairs) => pairs.iter_mut().flat_map(|(key, value)| [key, value]).collect(),
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => items.iter_mut().collect(),
        ExprKind::Slice { object, start, end } => {
            let mut parts = vec![object.as_mut()];
            parts.extend([start, end].into_iter().flatten().map(Box::as_mut));
            parts
        }
        ExprKind::Unary { expr: always, .. }
        | ExprKind::Cond { cond: always, .. }
        | ExprKind::Else { value: always, .. }
        | ExprKind::Match { subject: always, .. }
        | ExprKind::Try { value: always, .. } => vec![always.as_mut()],
        _ => Vec::new(),
    }
}

/// What of a call's callee runs before its arguments: a method's receiver, or a function worked
/// out, and nothing for a function by its name. A call's `[...]` names types, as
/// `db.query[int?](...)` does, where a `?` is an optional's, so it isn't looked in. A call through
/// an indexed value, as `handlers[at](x)`, isn't built natively yet; once it is, its index runs
/// first and has to be looked in.
fn runs_before_call(callee: &mut Expr) -> Option<&mut Expr> {
    if matches!(callee.kind, ExprKind::Name(_) | ExprKind::Variant(_)) {
        return None;
    }
    if !matches!(callee.kind, ExprKind::Index { .. } | ExprKind::Attr { .. }) {
        return Some(callee);
    }
    match &mut callee.kind {
        ExprKind::Index { object, .. } => runs_before_call(object),
        ExprKind::Attr { object, .. } => Some(object.as_mut()),
        _ => None,
    }
}

/// Whether a `?` that's hoisted is in `value`, where it always runs.
fn holds_try(value: &mut Expr) -> bool {
    matches!(value.kind, ExprKind::Try { context: None, .. }) || runs_first(value).into_iter().any(holds_try)
}

/// Whether `value` is written out, and so is the same whenever it's worked out: a `?` to its
/// right needn't go after it. A name or a field is read where it's written, even so, as a `?` to
/// its right may change it, as `count + bump(ref count)?` does.
fn written_out(value: &Expr) -> bool {
    matches!(
        value.kind,
        ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Duration(_)
            | ExprKind::Bytes(_)
            | ExprKind::Folder(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::None
            | ExprKind::Variant(_)
            | ExprKind::Lambda { .. }
    )
}

/// For each of `runs_first(value)`, whether it has to stay the place it is rather than be read
/// into a name: a call's receiver, which a method may change through `ref self`, and an argument
/// passed with `ref`.
fn places_first(value: &mut Expr) -> Vec<bool> {
    let ExprKind::Call { callee, args } = &mut value.kind else {
        return vec![false; runs_first(value).len()];
    };
    // A call's `[...]` names types, so the receiver is what's before them.
    let mut called: &Expr = callee;
    while let ExprKind::Index { object, .. } = &called.kind {
        called = object;
    }
    let receiver = matches!(called.kind, ExprKind::Attr { .. });
    let mut places = Vec::with_capacity(args.len() + 1);
    if runs_before_call(callee).is_some() {
        places.push(receiver);
    }
    places.extend(args.iter().map(|arg| arg.is_mut));
    places
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
        let mut failed = cleanup_copy(otherwise);
        failed.push(stmt(StmtKind::Fail(error), span));
        return failed;
    }
    let held = format!("failed__noted{}", here(span).start);
    let mut cleanup = noted(&cleanup_copy(otherwise), &held, span);
    cleanup.push(stmt(StmtKind::Break, span));
    let always = Expr { kind: ExprKind::Bool(true), span: here(span) };
    vec![
        assign(&held, error, true, None),
        stmt(StmtKind::While { cond: always, body: cleanup }, span),
        stmt(StmtKind::Fail(name(&held, span)), span),
    ]
}

/// A `lend`'s `else:` written in once more, as before each way the block fails, on spans of its
/// own, so what's found by span in one copy, which may be checked where another isn't, as in a
/// crash's function, is never another's (#2056, #2208): the program's text in it, and the
/// standard library's, a source's own copied in already among it. As it is only when spans have
/// run out, which refuses the program at the end of the round (`program_copies_ran_out`,
/// `std_copies_ran_out`).
fn cleanup_copy(cleanup: &[Stmt]) -> Vec<Stmt> {
    let mut copy = cleanup.to_vec();
    match copied_apart(Region::Program, &mut [&mut copy]).and_then(|()| std_copied_apart(&mut copy)) {
        Some(()) => copy,
        None => cleanup.to_vec(),
    }
}

/// Moves all of the standard library's text in a copy to spans of its own below every copy made so
/// far: the library's own text, and copies of it written in already. Each stretch of it keeps where
/// its parts are against each other, and the stretches stay in their order. Nothing, when spans
/// have run out.
fn std_copied_apart(copy: &mut [Stmt]) -> Option<()> {
    let mut spans = Vec::new();
    fennec_syntax::visit::in_stmts(copy, &mut |expr| {
        if expr.span.in_prelude() {
            spans.push((expr.span.start, expr.span.end.max(expr.span.start)));
        }
    });
    let stretches = stretched(spans);
    let needed: u64 = stretches.iter().map(|(start, end)| u64::from(end - start) + 1).sum();
    let Ok(needed) = u32::try_from(needed) else {
        STD_COPIES_RAN_OUT.with(|ran_out| ran_out.set(true));
        return None;
    };
    if needed == 0 {
        return Some(());
    }
    let mut next = std_copies_reserved(needed)?;
    let mut laid = Vec::with_capacity(stretches.len());
    for (start, end) in stretches {
        laid.push((start, end, next));
        next += end - start + 1;
    }
    fennec_syntax::visit::in_stmts(copy, &mut |expr| {
        if !expr.span.in_prelude() {
            return;
        }
        let after = laid.partition_point(|(start, ..)| *start <= expr.span.start);
        if let Some((start, _, to)) = after.checked_sub(1).and_then(|at| laid.get(at)) {
            let moved = |at: u32| at - start + to;
            expr.span =
                Span { start: moved(expr.span.start), end: moved(expr.span.end.max(expr.span.start)) };
        }
    });
    Some(())
}

/// `if failed is not None:` the `lend`'s `else:`, then failing with what's held, which a block's
/// failures leave their loop holding.
fn failing_if_held(failed: &str, otherwise: &[Stmt], span: Span) -> Stmt {
    let failing = failing_with(name(failed, span), otherwise, span);
    stmt(StmtKind::If { branches: vec![(is_held(failed, span), failing)], otherwise: None }, span)
}

/// `if failed is not None: break`: a loop inside a block leaving the block's once a failure in
/// it is held.
fn leave_if_held(failed: &str, span: Span) -> Stmt {
    stmt(
        StmtKind::If { branches: vec![(is_held(failed, span), vec![stmt_break(span)])], otherwise: None },
        span,
    )
}

/// `failed is not None`: whether a failure is held there.
fn is_held(failed: &str, span: Span) -> Expr {
    let none = Box::new(Expr { kind: ExprKind::None, span: here(span) });
    Expr {
        kind: ExprKind::Binary {
            op: fennec_syntax::BinOp::IsNot,
            lhs: Box::new(name(failed, span)),
            rhs: none,
        },
        span: here(span),
    }
}

/// A cleanup that ends by failing, with each of its `fail`s leaving a loop it runs in once, and
/// one `fail` after: whatever cleans up after it is then written in once, not before each.
fn funneled(cleanup: Vec<Stmt>, failure: TypeExpr, span: Span) -> Vec<Stmt> {
    let failed = format!("failed__cleanup{}", here(span).start);
    let mut body = cleanup;
    funnel_fails(&mut body, &failed, span);
    body.push(stmt(StmtKind::Break, span));
    let none = Expr { kind: ExprKind::None, span: here(span) };
    let always = Expr { kind: ExprKind::Bool(true), span: here(span) };
    vec![
        assign(&failed, none, true, Some(failure)),
        stmt(StmtKind::While { cond: always, body }, span),
        stmt(StmtKind::Assert(is_held(&failed, span)), span),
        stmt(StmtKind::Fail(name(&failed, span)), span),
    ]
}

/// Each `fail` in a cleanup as holding its failure in `failed` and leaving the loop it runs in,
/// and each loop inside holding one as leaving that one too. Says how many there were.
fn funnel_fails(body: &mut Vec<Stmt>, failed: &str, span: Span) -> usize {
    let mut made = 0;
    let mut at = 0;
    while at < body.len() {
        let Some(each) = body.get_mut(at) else { break };
        // A function written inside fails by itself, not the cleanup.
        if matches!(each.kind, StmtKind::Def(_)) {
            at += 1;
            continue;
        }
        for value in stmt_exprs(each) {
            outer_cases(value, &mut |cases| made += funnel_fails(cases, failed, span));
        }
        let replaced = match &mut each.kind {
            StmtKind::Fail(failure) => {
                made += 1;
                Some(vec![assign(failed, failure.clone(), false, None), stmt_break(span)])
            }
            StmtKind::If { branches, otherwise } => {
                for (_, branch) in branches {
                    made += funnel_fails(branch, failed, span);
                }
                if let Some(branch) = otherwise {
                    made += funnel_fails(branch, failed, span);
                }
                None
            }
            StmtKind::For { iter, .. } if crate::parallel::parallel_items(iter).is_some() => None,
            StmtKind::While { body: inner, .. } | StmtKind::For { body: inner, .. } => {
                let inside = funnel_fails(inner, failed, span);
                made += inside;
                (inside > 0).then(|| {
                    let leave = vec![stmt_break(span)];
                    let leave = stmt(
                        StmtKind::If { branches: vec![(is_held(failed, span), leave)], otherwise: None },
                        span,
                    );
                    vec![each.clone(), leave]
                })
            }
            StmtKind::With { body: inner, .. } | StmtKind::OnFailure { body: inner, .. } => {
                made += funnel_fails(inner, failed, span);
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
    made
}

/// Whether a cleanup statement can fail: a `fail`, or a whole line that's a `?`, here or in an
/// `if` inside it. A `match` isn't looked into: a `with launch`'s cleanup fails from one with the
/// task's failure, which is why the block failed, so it fails the block rather than being a note.
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
        // Each name it's split into is a hidden one, with the uses in its sight in the block:
        // written as itself, the split would assign a name of the caller's of that name.
        let hidden: Vec<String> =
            names.iter().map(|named| format!("{}__given{}", named.name, at.start)).collect();
        for (named, written) in names.iter().zip(&hidden) {
            fennec_syntax::sight::rename_in_sight(block, &named.name, written);
        }
        let targets = names.iter().zip(&hidden).map(|(named, written)| name(written, named.span)).collect();
        let target = Expr { kind: ExprKind::Tuple(targets), span: here(at) };
        let kind = StmtKind::Assign { target, op: None, value: name(place, at), declare_mut: true, ty: None };
        block.insert(0, stmt(kind, at));
    } else {
        // Each use of the `with`'s name is the source's own name for what it lends, but not a use
        // under something in the block that names it again. When that's a name of the caller's,
        // something in the block that names it again would take those uses: it's hidden first.
        let inner = format!("given{}", at.start);
        fennec_syntax::sight::hide_bound(block, &[(place.to_owned(), None)], &|_, name| {
            (name == place).then(|| format!("{name}__{inner}"))
        });
        fennec_syntax::sight::rename_in_sight(block, held, place);
    }
}

/// Where no failure is held to run the source's cleanup once after the block, as in a `test`, the
/// cleanup is written in before each failure inside the block, under what the block binds there.
/// So what the block binds of a name the cleanup reads is hidden first, with the uses in its sight,
/// and the cleanup reads what the name means at the `with`: a name of the caller's that a `ref`
/// parameter's uses are written as.
fn cleanup_reads_kept(block: &mut [Stmt], cleanup: &[Stmt], at: Span) {
    let mut read = BTreeSet::new();
    fennec_syntax::visit::in_stmts(&mut cleanup.to_vec(), &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind {
            read.insert(name.clone());
        }
    });
    // Each is in sight at the `with`, so assigning it in the block changes it there: only what binds
    // it again is hidden. A nested function's and an arrow's parameters keep theirs: the cleanup is
    // never written inside either, and a nested function's are named by calls.
    let outside: Vec<(String, Option<String>)> = read.iter().map(|name| (name.clone(), None)).collect();
    let inner = format!("kept{}", at.start);
    fennec_syntax::sight::hide_bound(block, &outside, &|binder, name| {
        let surrounds = !matches!(binder, Binder::Parameter | Binder::Arrow);
        (surrounds && read.contains(name)).then(|| format!("{name}__{inner}"))
    });
}

/// A cleanup that unmarks what a crash runs before it runs, when there's one.
fn unmarked_first(crash: Option<&(Stmt, Stmt, Stmt)>, otherwise: &[Stmt]) -> Vec<Stmt> {
    match crash {
        Some((_, _, done)) => std::iter::once(done.clone()).chain(otherwise.iter().cloned()).collect(),
        None => otherwise.to_vec(),
    }
}

/// Since 2026-11-11 a program can't write `with src else fail E:`, which `else` on a failing
/// call refuses, so nothing reaches this from what a program writes; it's kept until lowering is
/// next reworked.
///
/// A source's body as `with src else fail E:` writes it in: each `fail` does what the `else` does
/// instead, and so does each `?`, through `else`. A `?` in an arrow function passes the arrow's
/// own failure up, not the source's, so it stays.
fn fail_instead(body: &mut [Stmt], instead: &Expr) {
    instead_of_fails(body, instead);
    let leaving = Box::new(instead.clone());
    let mut in_arrows = BTreeSet::new();
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
                let kind = ExprKind::Else { value: value.clone(), default: leaving.clone(), except: false };
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

/// What a crash in a `with` block runs: the source's `except:` cleanup as a function taking
/// nothing and giving back how it failed, as text, or nothing, the call that marks it to run, and
/// the call that unmarks it. `None` with no cleanup.
fn on_crash(otherwise: &[Stmt], at: Span) -> Option<(Stmt, Stmt, Stmt)> {
    if otherwise.is_empty() {
        return None;
    }
    let held = format!("{ON_CRASH}{}", here(at).start);
    let body = as_written(otherwise, &held, at);
    let def = FnDef {
        name: held.clone(),
        generics: Vec::new(),
        params: Vec::new(),
        ret: Some(TypeExpr { name: "str".to_owned(), args: Vec::new(), optional: false, span: here(at) }),
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

/// A cleanup as a crash runs it, giving back how it failed, as text, for the runtime to note on
/// the crash, or `""`. The cleanup is a function of its own, written as it is, and called: a
/// failure passing up out of it ends it and is given back, as it would end it for a failed block.
/// That's a `?`, a `fail`, or a `with` whose source fails as its block ends. Nothing in it is
/// rewritten, so it checks as it did where it was written, whatever's in it: a `with launch`, a
/// source, a `lock`, a `parallel` loop, a `?` in a comprehension. A `return` in it ends it with
/// nothing to say.
fn as_written(cleanup: &[Stmt], held: &str, at: Span) -> Vec<Stmt> {
    let inner = format!("{held}_body");
    let def = FnDef {
        name: inner.clone(),
        generics: Vec::new(),
        params: Vec::new(),
        ret: None,
        arrow: None,
        fails: None,
        body: cleanup_copy(cleanup),
        annotations: Vec::new(),
        span: here(at),
    };
    let call = Expr {
        kind: ExprKind::Call { callee: Box::new(name(&inner, at)), args: Vec::new() },
        span: here(at),
    };
    // A cleanup with no `?` or `fail` of its own can't fail, and `except` on its call would be
    // refused as handling nothing.
    let ran = if can_fail(cleanup) {
        let handled =
            ExprKind::Else { value: Box::new(call), default: Box::new(ends_cleanup(at, None)), except: true };
        Expr { kind: handled, span: here(at) }
    } else {
        call
    };
    let nothing = Expr { kind: ExprKind::Str(String::new()), span: here(at) };
    vec![stmt(StmtKind::Def(def), at), stmt_value(ran), stmt(StmtKind::Return(Some(nothing)), at)]
}

/// Whether `body` can fail: it has a `?` or a `fail` of its own, not one in a function or lambda
/// written in it, which passes up from that.
fn can_fail(body: &[Stmt]) -> bool {
    let mut own = body.to_vec();
    functions_emptied(&mut own);
    let mut found = fails_in(&mut own);
    fennec_syntax::visit::in_stmts(&mut own, &mut |expr| match &mut expr.kind {
        ExprKind::Try { .. } => found = true,
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                found |= fails_in(&mut arm.body);
            }
        }
        _ => {}
    });
    found
}

/// Whether `body` has a `fail` statement of its own, in its blocks but not in a function
/// written in it.
fn fails_in(body: &mut [Stmt]) -> bool {
    body.iter_mut().any(|each| match &each.kind {
        StmtKind::Fail(_) => true,
        StmtKind::Def(_) => false,
        _ => blocks_in(each).into_iter().any(|block| fails_in(block)),
    })
}

/// The text a crash's cleanup gives back for `failure`: how it's printed.
fn said(failure: Expr, at: Span) -> Expr {
    let part = fennec_syntax::FStrPart::Expr { expr: failure, spec: None };
    Expr { kind: ExprKind::FStr(vec![part]), span: here(at) }
}

/// `else as why: return f"{why}"`, as the parser writes it: one case, over a value nothing wrote,
/// that leaves. For a `? from Context(...)`, what's given back is the context, which is what
/// would pass up.
fn ends_cleanup(at: Span, context: Option<Expr>) -> Expr {
    let subject = Box::new(Expr { kind: ExprKind::Bool(false), span: here(at) });
    let why = format!("crash__why{}", here(at).start);
    let failure = context.unwrap_or_else(|| name(&why, at));
    let leave = stmt(StmtKind::Return(Some(said(failure, at))), at);
    let arm = MatchArm { pattern: Pattern::Bind(why), guard: None, body: vec![leave], span: here(at) };
    Expr { kind: ExprKind::Match { subject, arms: vec![arm] }, span: here(at) }
}

/// `body` with the bodies of the functions and lambdas written in it emptied, at any depth, so
/// what's left is its own: their `?`, `fail` and `return` are theirs.
fn functions_emptied(body: &mut [Stmt]) {
    defs_emptied(body);
    fennec_syntax::visit::in_stmts(body, &mut |expr| match &mut expr.kind {
        ExprKind::Lambda { body: inner, .. } => {
            **inner = Expr { kind: ExprKind::None, span: inner.span };
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                defs_emptied(&mut arm.body);
            }
        }
        _ => {}
    });
}

/// The statement blocks directly inside a statement, to change.
fn blocks_in(each: &mut Stmt) -> Vec<&mut Vec<Stmt>> {
    match &mut each.kind {
        StmtKind::If { branches, otherwise } => {
            branches.iter_mut().map(|(_, body)| body).chain(otherwise.as_mut()).collect()
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => vec![body],
        StmtKind::Lend { otherwise, .. } => vec![otherwise],
        StmtKind::OnFailure { cleanup, body, .. } => vec![cleanup, body],
        _ => Vec::new(),
    }
}

fn defs_emptied(body: &mut [Stmt]) {
    for each in body {
        if let StmtKind::Def(def) = &mut each.kind {
            def.body.clear();
            continue;
        }
        for block in blocks_in(each) {
            defs_emptied(block);
        }
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

/// A `ref` case's parts, one for each of its fields in their order, as the case's fields made
/// from what they bind, to put it back with. A `ref` part is bound as a name the body may change,
/// and a `_` to a hidden name, so it's put back as it was.
fn parts_bound(
    args: &mut [Pattern],
    fields: &[String],
    made: usize,
    at: Span,
) -> Option<Vec<fennec_syntax::Arg>> {
    let mut written = Vec::new();
    for (at_field, (arg, field)) in args.iter_mut().zip(fields).enumerate() {
        let value = match arg {
            Pattern::RefBind(held) => {
                let value = name(held, at);
                *arg = Pattern::MutBind(std::mem::take(held));
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
        written.push(fennec_syntax::Arg { name: Some(field.clone()), is_mut: false, value });
    }
    Some(written)
}

/// Whether a `ref` case can be written in: its parts fit its fields, and each is a `ref`, a name,
/// a `_` or a literal.
fn rewritable(pattern: &Pattern, fields: &[String]) -> bool {
    let Pattern::Variant { args, fields: named, .. } = pattern else { return false };
    let kinds = args.iter().all(|arg| {
        matches!(arg, Pattern::RefBind(_) | Pattern::Bind(_) | Pattern::Wildcard | Pattern::Literal(_))
    });
    kinds && in_fields_order(args.clone(), named, fields).is_some()
}

/// A case's parts in its fields' order, one for each field: a part written by name goes in its
/// field's place, and a field no part is written for, by name or past the last by position, is a
/// `_`. Nothing when there are more parts than fields, or one names no field.
fn in_fields_order(args: Vec<Pattern>, named: &[String], fields: &[String]) -> Option<Vec<Pattern>> {
    if args.len() > fields.len() {
        return None;
    }
    if named.is_empty() {
        let mut placed = args;
        placed.resize(fields.len(), Pattern::Wildcard);
        return Some(placed);
    }
    let mut placed = vec![Pattern::Wildcard; fields.len()];
    for (field, arg) in named.iter().zip(args) {
        let at = fields.iter().position(|known| known == field)?;
        *placed.get_mut(at)? = arg;
    }
    Some(placed)
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
            def opened(n: int) -> with int fails Boom:\n    lend n except:\n        pass\n\n\
            def main(stdout: Stdout) fails Boom:\n    with opened(1)? as first:\n        stdout.print(first)\n";
        let mut module = fennec_syntax::parse(source).unwrap();
        let checked = crate::checker::Checker::new(&module).run(crate::checker::Scope::Everything).unwrap();
        let (launches, matches) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
        assert!(super::lower(
            &mut module,
            &checked.sources,
            &launches,
            (&matches, &std::collections::BTreeMap::new())
        ));
        assert!(!super::lower(
            &mut module,
            &checked.sources,
            &launches,
            (&matches, &std::collections::BTreeMap::new())
        ));
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

    /// A `match` inside another's case with the same span as the outer one is still told apart
    /// from it: the outer's cases are each passed once, and the inner's are left to the outer's
    /// case, rather than both being dropped as one `match` taken for inner. The parser no longer
    /// gives the cases under `except why:` the outer's span, which is how this was found, so the
    /// test builds the shared span by hand to keep any parse that does it again from losing
    /// cleanups.
    #[test]
    fn a_match_inside_another_s_case_with_its_span_is_still_told_apart() {
        let source = "def f(a: int, b: int):\n    match a:\n        case 1:\n            match b:\n                case 2:\n                    pass\n                case _:\n                    pass\n        case _:\n            pass\n";
        let mut module = fennec_syntax::parse(source).unwrap();
        let Some(fennec_syntax::Item::Function(def)) = module.items.first_mut() else {
            panic!("no function in {source}");
        };
        let Some(fennec_syntax::StmtKind::Expr(outer)) = def.body.first_mut().map(|stmt| &mut stmt.kind)
        else {
            panic!("no match in {source}");
        };
        let span = outer.span;
        // The inner `match` gets the outer one's span.
        let fennec_syntax::ExprKind::Match { arms, .. } = &mut outer.kind else { panic!("not a match") };
        let Some(fennec_syntax::StmtKind::Expr(inner)) =
            arms.first_mut().and_then(|arm| arm.body.first_mut()).map(|stmt| &mut stmt.kind)
        else {
            panic!("no inner match");
        };
        inner.span = span;
        let mut passed = Vec::new();
        super::outer_cases(outer, &mut |cases| passed.push(cases.len()));
        assert_eq!(passed, [1, 1], "each of the outer match's two cases once, and none of the inner's");
    }

    /// A copy of the standard library's text is moved below every copy made so far, each stretch
    /// keeping where its parts are against each other, and two copies of the same text never share
    /// a span. One that finds no room left says so, for the round to refuse the program, rather
    /// than going below where copies may go.
    #[test]
    fn a_copy_of_std_text_gets_spans_of_its_own_or_says_there_is_no_room() {
        use fennec_syntax::{Expr, ExprKind, Span, Stmt, StmtKind};
        let at = |start: u32, end: u32| Span { start: Span::PRELUDE + start, end: Span::PRELUDE + end };
        let call = || Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::Call {
                    callee: Box::new(Expr { kind: ExprKind::Name("close".to_owned()), span: at(10, 15) }),
                    args: Vec::new(),
                },
                span: at(10, 17),
            }),
            span: at(10, 17),
        };
        let spans = |stmts: &mut [Stmt]| {
            let mut found = Vec::new();
            fennec_syntax::visit::in_stmts(stmts, &mut |expr| found.push(expr.span));
            found
        };
        super::COPIES.with(|copies| copies.set(u32::MAX));
        let (mut one, mut two) = ([call()], [call()]);
        assert_eq!(super::std_copied_apart(&mut one), Some(()));
        assert_eq!(super::std_copied_apart(&mut two), Some(()));
        let (one, two) = (spans(&mut one), spans(&mut two));
        assert!(one.iter().all(|span| span.start >= super::COPIES_FROM), "{one:?}");
        assert!(one.iter().all(|span| !two.contains(span)), "{one:?} {two:?}");
        assert_eq!(one[0].end - one[0].start, 7, "the call keeps its length");
        super::COPIES.with(|copies| copies.set(super::COPIES_FROM + 3));
        assert_eq!(super::std_copied_apart(&mut [call()]), None);
        assert!(super::std_copies_ran_out());
    }
}
