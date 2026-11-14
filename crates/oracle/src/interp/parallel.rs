//! `parallel(xs)`: rounds the compiled program runs on many threads, run here one after another,
//! in the list's order, where that gives what every order the threads could take gives.
//!
//! What's promised (`docs/decisions.md`): `[f(x) for x in parallel(xs)]` gives its results in the
//! list's order, a stream `(f(x) for x in parallel(xs))` is read in that order, and the failure a
//! call passes up is the first round's that fails, by position, since the runtime keeps the lowest
//! range's. Rounds after one that fails or breaks out may run or not, so they run here too, as
//! rounds that may not run, and anything they'd change is refused, as is one failing after a
//! `break`. What a round does that another round, or a stream's reader, could see in another
//! order is refused: printing, which follows real call order, the file system, a `shared` value
//! one round changes and another reaches, and a generator made outside the round. An int `sum` is
//! refused where some way of splitting it could overflow, and a float `sum` outright, since it
//! adds in a tree of its own.

use std::sync::atomic::Ordering;

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind, Span, Stmt};

use super::{Flow, Interp, Place, Run, Step, as_int, unsupported};
use crate::value::Value;

/// What runs beside something else now: a `parallel` call's rounds, or a `with launch`'s block
/// and its task.
pub(crate) enum Scope {
    Rounds(Call),
    Launch(super::tasks::Launched),
}

impl Scope {
    /// How many rounds had started when what runs now in it started, where a generator made
    /// before then can't be read, as something else beside it may read it too.
    fn reads_since(&self) -> u64 {
        match self {
            Scope::Rounds(call) => call.started,
            Scope::Launch(launched) => launched.reads_since(),
        }
    }
}

/// What a program reaches outside itself through, which what runs side by side can't share in
/// an order that's promised.
#[derive(Clone, Copy)]
pub(crate) enum Outside {
    Printing,
    Disk,
}

impl Outside {
    fn what(self) -> &'static str {
        match self {
            Outside::Printing => "printing",
            Outside::Disk => "the file system",
        }
    }
}

/// A `parallel` call running now.
pub(crate) struct Call {
    /// The round running now, by its position.
    round: usize,
    /// Whether the round running now may not run at all in the compiled program: one after a
    /// round that failed or broke out, which another thread may have started anyway.
    maybe: bool,
    /// Whether its rounds run alongside whoever reads what they make, as a stream's do.
    streamed: bool,
    /// The `shared` values its rounds have locked: which, in which round, and whether to change it.
    locked: Vec<(u64, usize, bool)>,
    /// How many rounds had started when the round running now started.
    started: u64,
    /// The number the first `shared` value made in the round running now got: one made in it is
    /// its own, which nothing else reaches.
    first_cell: u64,
}

/// The earliest a generator read now may have been made, inside what runs side by side now, or
/// `None` outside all of it.
pub(crate) fn reads_since(scopes: &[Scope]) -> Option<u64> {
    scopes.iter().map(Scope::reads_since).max()
}

/// `parallel(source, limit=n)`, as it's written: the source, and the limit, if it's given.
#[derive(Clone, Copy)]
pub(crate) struct Written {
    source: &'static Expr,
    limit: Option<&'static Expr>,
}

/// What a call's rounds made, `None` where one failed, broke out or made nothing, and what stopped
/// it, if anything did.
struct Ran<T> {
    made: Vec<Option<T>>,
    stop: Option<Flow>,
}

impl Interp {
    /// `parallel(source)`, when that's what an expression is, with the `limit=` it may take.
    pub(crate) fn parallel_call(&self, expr: &'static Expr) -> Run<Option<Written>> {
        let ExprKind::Call { callee, args } = &expr.kind else { return Ok(None) };
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        if name != "parallel" || self.is_bound(name) || self.program.functions.contains_key(name.as_str()) {
            return Ok(None);
        }
        let mut source = None;
        let mut limit = None;
        for arg in args {
            match arg.name.as_deref() {
                None if source.is_none() && !arg.is_mut => source = Some(&arg.value),
                Some("limit") if limit.is_none() => limit = Some(&arg.value),
                _ => return unsupported("`parallel` with these arguments"),
            }
        }
        match source {
            Some(source) => Ok(Some(Written { source, limit })),
            None => unsupported("`parallel` with no source"),
        }
    }

    /// The items a call's rounds take, in order, with its limit worked out after them.
    fn parallel_items(&mut self, call: Written) -> Run<Vec<Value>> {
        let items = match self.eval(call.source)? {
            Value::List(items) => items,
            Value::Table(table) => table.live().map(|(_, item)| item.clone()).collect(),
            other => return unsupported(format!("`parallel` over a {}", other.kind())),
        };
        self.parallel_limit(call)?;
        Ok(items)
    }

    /// A call's `limit=`, which says only how many rounds run at once, so it changes nothing here
    /// but has to be a count.
    fn parallel_limit(&mut self, call: Written) -> Run<()> {
        let Some(limit) = call.limit else { return Ok(()) };
        match self.eval(limit)? {
            Value::Int(n) if n > 0 => Ok(()),
            _ => unsupported("`parallel` with a `limit` that isn't a count"),
        }
    }

    /// Notes reaching outside the program, refusing it inside a `parallel` round, and in both a
    /// launched task and its block, where its order isn't promised.
    pub(crate) fn reach_out(&self, outside: Outside) -> Run<()> {
        let Ok(mut scopes) = self.shared.scopes.lock() else {
            return unsupported("a `parallel` call was lost");
        };
        for scope in scopes.iter_mut() {
            match scope {
                Scope::Rounds(_) => {
                    return unsupported(format!(
                        "{} inside a `parallel` round, whose order isn't promised",
                        outside.what()
                    ));
                }
                Scope::Launch(launched) => {
                    if !launched.reach_out(outside) {
                        return unsupported(format!(
                            "{} in both a launched task and its `with` block, whose order isn't promised",
                            outside.what()
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Notes a `lock` on the `shared` value `cell`, to change it or not, in everything running
    /// side by side now, refusing one whose outcome depends on the order they run in.
    pub(crate) fn note_lock(&self, cell: u64, writes: bool) -> Run<()> {
        let Ok(mut scopes) = self.shared.scopes.lock() else {
            return unsupported("a `parallel` call was lost");
        };
        for scope in scopes.iter_mut() {
            let rounds = match scope {
                Scope::Rounds(rounds) => rounds,
                Scope::Launch(launched) => {
                    if !launched.note_lock(cell, writes) {
                        return unsupported(
                            "a `shared` value a launched task or its `with` block changes and the other reaches",
                        );
                    }
                    continue;
                }
            };
            if cell >= rounds.first_cell {
                continue;
            }
            if rounds.streamed {
                return unsupported(
                    "a `lock` in a `parallel` stream's item, which runs alongside its reader",
                );
            }
            if rounds.maybe {
                if writes {
                    return unsupported("a `parallel` round that may not run changing a `shared` value");
                }
                continue;
            }
            let round = rounds.round;
            let clashes = rounds
                .locked
                .iter()
                .any(|&(held, by, wrote)| held == cell && by != round && (wrote || writes));
            if clashes {
                return unsupported(
                    "a `shared` value one `parallel` round changes and another reaches, in an order that isn't promised",
                );
            }
            rounds.locked.push((cell, round, writes));
        }
        Ok(())
    }

    fn push_call(&self, streamed: bool) -> Run<()> {
        let call = Call { round: 0, maybe: false, streamed, locked: Vec::new(), started: 0, first_cell: 0 };
        self.push_scope(Scope::Rounds(call))
    }

    pub(crate) fn push_scope(&self, scope: Scope) -> Run<()> {
        let Ok(mut scopes) = self.shared.scopes.lock() else {
            return unsupported("a `parallel` call was lost");
        };
        scopes.push(scope);
        Ok(())
    }

    pub(crate) fn pop_scope(&self) {
        if let Ok(mut scopes) = self.shared.scopes.lock() {
            scopes.pop();
        }
    }

    fn start_round(&self, round: usize, maybe: bool) -> Run<()> {
        let started = self.shared.rounds_started.fetch_add(1, Ordering::SeqCst) + 1;
        let Ok(mut scopes) = self.shared.scopes.lock() else {
            return unsupported("a `parallel` call was lost");
        };
        let Some(Scope::Rounds(call)) = scopes.last_mut() else {
            return unsupported("a `parallel` call was lost");
        };
        call.round = round;
        call.maybe = maybe;
        call.started = started;
        call.first_cell = self.shared.cells_made();
        Ok(())
    }

    /// Runs `each` for each of `count` rounds, in order. After one stops the call, by failing,
    /// crashing or breaking out, the rest run as rounds that may not run: their failures are
    /// passed over, since the first round's is the one passed up, and refused after a `break`,
    /// which would pass up a failure or not.
    fn rounds<T>(
        &mut self,
        count: usize,
        each: &mut dyn FnMut(&mut Interp, usize, bool) -> Run<T>,
    ) -> Run<Ran<T>> {
        self.push_call(false)?;
        let ran = self.each_round(count, each);
        self.pop_scope();
        ran
    }

    fn each_round<T>(
        &mut self,
        count: usize,
        each: &mut dyn FnMut(&mut Interp, usize, bool) -> Run<T>,
    ) -> Run<Ran<T>> {
        let mut made = Vec::with_capacity(count);
        let mut stop: Option<Flow> = None;
        for at in 0..count {
            let maybe = stop.is_some();
            self.start_round(at, maybe)?;
            match each(self, at, maybe) {
                Ok(value) => made.push(Some(value)),
                Err(flow @ (Flow::Unsupported(_) | Flow::Abandoned)) => return Err(flow),
                Err(Flow::Return(_)) => return unsupported("a `return` inside a `parallel` round"),
                Err(flow) => {
                    made.push(None);
                    match &stop {
                        None => stop = Some(flow),
                        Some(Flow::Break) if !matches!(flow, Flow::Break | Flow::Continue) => {
                            return unsupported("a `parallel` round that may run after a `break` failing");
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        Ok(Ran { made, stop })
    }

    /// `for names in parallel(source):`, each round with its item.
    pub(crate) fn for_parallel(
        &mut self,
        names: &'static [String],
        call: Written,
        body: &'static [Stmt],
    ) -> Run<()> {
        let items = self.parallel_items(call)?;
        let ran = self.rounds(items.len(), &mut |interp, at, _| {
            let item = items.get(at).cloned().unwrap_or(Value::Unit);
            let mark = interp.mark();
            let ran = interp.bind_fresh(names, item).and_then(|()| interp.stmts(body));
            interp.restore(mark);
            match ran {
                Err(Flow::Continue) => Ok(()),
                other => other,
            }
        })?;
        match ran.stop {
            None | Some(Flow::Break) => Ok(()),
            Some(flow) => Err(flow),
        }
    }

    /// `for ref name in parallel(source):`, each round changing its own item where it is. A round
    /// that may not run is refused if it changes its item.
    pub(crate) fn for_ref_parallel(
        &mut self,
        name: &str,
        call: Written,
        body: &'static [Stmt],
        span: Span,
    ) -> Run<()> {
        let Some(place) = self.place_of(call.source)? else {
            return unsupported("`for ref` over `parallel` of something that isn't a place");
        };
        let positions: Vec<Value> = match self.read_place(&place, span)? {
            Value::List(items) => (0..items.len()).map(|at| Value::Int(as_int(at))).collect(),
            Value::Table(table) => table.live().map(|(id, _)| Value::Id(id)).collect(),
            other => return unsupported(format!("`for ref` over `parallel` of a {}", other.kind())),
        };
        self.parallel_limit(call)?;
        let ran = self.rounds(positions.len(), &mut |interp, at, maybe| {
            let position = positions.get(at).cloned().unwrap_or(Value::Unit);
            interp.ref_round(&place.then(Step::At(position)), name, body, maybe, span)
        })?;
        match ran.stop {
            None | Some(Flow::Break) => Ok(()),
            Some(flow) => Err(flow),
        }
    }

    /// One round of `for ref` over `parallel`, with its item where it is.
    fn ref_round(
        &mut self,
        item: &Place,
        name: &str,
        body: &'static [Stmt],
        maybe: bool,
        span: Span,
    ) -> Run<()> {
        let before = if maybe { Some(self.read_place(item, span)?) } else { None };
        let mark = self.mark();
        let ran = self.bind_ref(name, item.clone()).and_then(|()| self.stmts(body));
        self.restore(mark);
        if let Err(flow @ (Flow::Unsupported(_) | Flow::Abandoned)) = ran {
            return Err(flow);
        }
        if let Some(before) = before
            && !crate::value::same(&before, &self.read_place(item, span)?)
        {
            return unsupported("a `parallel` round that may not run changing its item");
        }
        match ran {
            Err(Flow::Continue) => Ok(()),
            other => other,
        }
    }

    /// What each round of `[elem for names in parallel(source) if cond]` makes, by position.
    fn parallel_made(
        &mut self,
        elem: &'static Expr,
        names: &'static [String],
        call: Written,
        cond: Option<&'static Expr>,
    ) -> Run<Ran<Option<Value>>> {
        let items = self.parallel_items(call)?;
        self.rounds(items.len(), &mut |interp, at, _| {
            let item = items.get(at).cloned().unwrap_or(Value::Unit);
            interp.made_from(elem, names, item, cond)
        })
    }

    /// What `elem` makes of an item, or `None` when `cond` leaves it out.
    fn made_from(
        &mut self,
        elem: &'static Expr,
        names: &'static [String],
        item: Value,
        cond: Option<&'static Expr>,
    ) -> Run<Option<Value>> {
        let mark = self.mark();
        let made = self.bind_fresh(names, item).and_then(|()| {
            let keep = match cond {
                Some(cond) => self.truth(cond)?,
                None => true,
            };
            if keep { self.eval(elem).map(Some) } else { Ok(None) }
        });
        self.restore(mark);
        made
    }

    /// `[elem for names in parallel(source) if cond]`: the rounds' results, in the list's order.
    pub(crate) fn parallel_list(
        &mut self,
        elem: &'static Expr,
        names: &'static [String],
        call: Written,
        cond: Option<&'static Expr>,
    ) -> Run<Vec<Value>> {
        let ran = self.parallel_made(elem, names, call, cond)?;
        if let Some(stop) = ran.stop {
            return Err(stop);
        }
        let made: Vec<Value> = ran.made.into_iter().flatten().flatten().collect();
        if made.len() > super::MOST_ITEMS {
            return unsupported("ran too long: a collection too big");
        }
        Ok(made)
    }

    /// `sum(elem for names in parallel(source) if cond)`, with the start it may take. Its parts
    /// add in a tree the runtime picks, so an int sum is modeled only where no way of adding them
    /// could overflow: where the positive parts add up without overflowing, and the negative ones
    /// too, every part of every tree lies between those two.
    pub(crate) fn parallel_sum(
        &mut self,
        generator: &'static Expr,
        call: Written,
        start: Option<&Value>,
        span: Span,
    ) -> Run<Value> {
        let ExprKind::Comprehension { elem, names, cond, .. } = &generator.kind else {
            return unsupported("a generator that isn't one");
        };
        match self.program.type_at(span) {
            Some(Type::Int) => {}
            Some(Type::Float) => {
                return unsupported("a float `sum` over `parallel`, which adds in a tree of its own");
            }
            _ => return unsupported("a `sum` over `parallel` of something other than ints"),
        }
        let ran = self.parallel_made(elem, names, call, cond.as_deref())?;
        let (mut up, mut down) = (0_i64, 0_i64);
        for part in start.into_iter().chain(ran.made.iter().flatten().flatten()) {
            let Value::Int(n) = part else {
                return unsupported(format!("`sum` over `parallel` of a {}", part.kind()));
            };
            let total = if *n >= 0 { &mut up } else { &mut down };
            match total.checked_add(*n) {
                Some(added) => *total = added,
                None => {
                    return unsupported("an int `sum` over `parallel` that may overflow, by how it's split");
                }
            }
        }
        if let Some(stop) = ran.stop {
            return Err(stop);
        }
        Ok(Value::Int(up + down))
    }

    /// In a stream's generator, `(elem for names in parallel(source) if cond)`: each item worked
    /// out as a round of its own, which runs alongside the reader, and handed to it in order. A
    /// failure is the reader's when it reaches the item that failed, and an item past where it
    /// stops reading is never worked out, as one the runtime works out early is passed over.
    pub(crate) fn yield_parallel(
        &mut self,
        elem: &'static Expr,
        names: &'static [String],
        call: Written,
        cond: Option<&'static Expr>,
    ) -> Run<()> {
        let items = self.parallel_items(call)?;
        for (at, item) in items.into_iter().enumerate() {
            self.push_call(true)?;
            let made = self.start_round(at, false).and_then(|()| self.made_from(elem, names, item, cond));
            self.pop_scope();
            if let Some(made) = made? {
                self.yield_value(made)?;
            }
        }
        Ok(())
    }
}
