//! `parallel(xs)`: rounds the compiled program runs on many threads, run here one after another,
//! in the list's order, where that gives what every order the threads could take gives, until one
//! waits on a channel: then the rounds after it are handed to a thread of their own (`actors.rs`).
//!
//! What's promised (`docs/decisions.md`): `[f(x) for x in parallel(xs)]` gives its results in the
//! list's order, a stream `(f(x) for x in parallel(xs))` is read in that order, and the failure a
//! call passes up is the first round's that fails, by position, since the runtime keeps the lowest
//! range's. Rounds after one that fails or breaks out may run or not, so they run here too, as
//! rounds that may not run, and anything they'd change, or a channel they'd use, is refused, as
//! is one failing after a `break`. A `for` over `parallel` whose body has a `return` in it runs
//! as a plain loop over the source instead, as compiled code runs it and the docs say: one round
//! after another, the `return` leaving the function. What a round does that another round, or a stream's reader,
//! could see in another order is refused: printing, which follows real call order, the file
//! system, a `shared` value one round changes and another reaches, and a generator made outside
//! the round. Each round is one of its own to the channels it uses (`actors.rs`), so which
//! round's message comes first is tried every way, as two tasks' is; of a channel made outside
//! it, a close is refused, as is any use in a stream's item. A `sum` adds in the runtime's fixed
//! tree where compiled code lifts it into ranges, and in order, as a plain `sum` does, where it
//! doesn't, so a float `sum` gives the same bits and an int `sum` overflows where compiled code's
//! does.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind, Span, Stmt, StmtKind, visit};

use super::calls::ArgVal;
use super::{Flow, Interp, Place, Run, Step, as_int, crash, unsupported};
use crate::value::{Held, Value};

/// What runs beside something else now: a `parallel` call's rounds, or a `with launch`'s block
/// and its task.
pub(crate) enum Scope {
    Rounds(Call),
    Launch(super::tasks::Launched),
}

impl Scope {
    /// A copy for another thread that runs inside it too: a call's rounds handed on, which share
    /// what its rounds have locked, as the two sides of a launch do.
    pub(crate) fn copy(&self) -> Scope {
        match self {
            Scope::Rounds(call) => Scope::Rounds(call.clone()),
            Scope::Launch(launched) => Scope::Launch(launched.clone()),
        }
    }

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
    pub(crate) fn what(self) -> &'static str {
        match self {
            Outside::Printing => "printing",
            Outside::Disk => "the file system",
        }
    }
}

/// A `parallel` call running now.
#[derive(Clone)]
pub(crate) struct Call {
    /// The round running now, by its position.
    round: usize,
    /// Whether the round running now may not run at all in the compiled program: one after a
    /// round that failed or broke out, which another thread may have started anyway.
    maybe: bool,
    /// Whether its rounds run alongside whoever reads what they make, as a stream's do.
    streamed: bool,
    /// The `shared` values its rounds have locked: which, in which round, and whether to change
    /// it, shared with a thread its rounds are handed on to.
    locked: Arc<Mutex<Vec<(u64, usize, bool)>>>,
    /// How many rounds had started when the round running now started.
    started: u64,
    /// The number the first `shared` value made in the round running now got: one made in it is
    /// its own, which nothing else reaches.
    first_cell: u64,
    /// How many rounds it has.
    count: usize,
    /// The number the first channel made in the round running now got: one made in it is its own
    /// too.
    pub(crate) first_channel: i64,
    /// Whether the rounds after the one running now were handed to a thread of their own, where
    /// they run beside it.
    pub(crate) handed: bool,
}

/// What each round of a `parallel` call runs, where its rounds can be handed to another thread:
/// all of it the call's own, so any thread can run any round from it.
pub(crate) enum Work {
    /// `for names in parallel(...)`: the body, with each item.
    For { names: &'static [String], body: &'static [Stmt], items: Vec<Value> },
    /// A comprehension's element, for each item its condition keeps.
    /// A dict comprehension's `value` too, each round making its key and value as a pair.
    Made {
        elem: &'static Expr,
        value: Option<&'static Expr>,
        names: &'static [String],
        cond: Option<&'static Expr>,
        items: Vec<Value>,
    },
}

/// A `parallel` call whose rounds this thread runs, the innermost last, and whether the rounds
/// after the one running now have been handed to a thread of their own (`actors.rs`).
pub(crate) struct Handing {
    /// What each round runs, or `None` where its rounds can't be handed on: `for ref`'s, which
    /// change the caller's items where they are.
    pub(crate) work: Option<Arc<Work>>,
    pub(crate) count: usize,
    /// The round running now, and whether it may not run.
    pub(crate) round: usize,
    pub(crate) maybe: bool,
    /// How many frames the call's caller had, and the mark of the frame its rounds name things
    /// in: what a thread its rounds are handed to is given a copy of.
    pub(crate) frames: usize,
    pub(crate) mark: (usize, usize),
    /// Where the call is in what runs side by side, and how many rounds it's inside.
    pub(crate) scope: usize,
    pub(crate) level: usize,
    pub(crate) handed: Option<Handed>,
}

/// The thread a call's rounds were handed to, and where it leaves what they made.
pub(crate) struct Handed {
    pub(crate) slot: usize,
    pub(crate) done: Arc<Mutex<Option<RoundsRan>>>,
}

/// What rounds made, from the first one run on a thread on, and the numbers of those that used
/// a channel.
pub(crate) type RoundsRan = (Ran, Vec<usize>);

/// What a round does with a channel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelUse {
    Send,
    Receive,
    Close,
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

impl Written {
    /// What the call goes over, for a loop that runs as a plain one over it.
    pub(crate) fn source(self) -> &'static Expr {
        self.source
    }
}

/// Whether a `parallel` loop's `body` has a `return` in it, outside the functions written inside
/// it: in its blocks, a `match`'s cases and an `except` block included. Compiled code never lifts
/// such a body into ranges (`liftable` in the IR's lowering, which this walks as it does), and
/// runs it as a plain loop.
pub(crate) fn returns_in(body: &[Stmt]) -> bool {
    let mut written = body.to_vec();
    without_functions(&mut written);
    returns_in_blocks(&written)
}

fn returns_in_blocks(body: &[Stmt]) -> bool {
    let in_blocks = body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Return(_) => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, block)| returns_in_blocks(block))
                || otherwise.as_deref().is_some_and(returns_in_blocks)
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => returns_in_blocks(body),
        _ => false,
    });
    in_blocks || {
        let mut cases = Vec::new();
        visit::in_stmts(&mut body.to_vec(), &mut |seen| {
            if let ExprKind::Match { arms, .. } = &seen.kind {
                cases.extend(arms.iter().map(|arm| arm.body.clone()));
            }
        });
        cases.iter().any(|case| returns_in_blocks(case))
    }
}

/// Takes the functions written inside `body` out of it, at any depth, those in a `match`'s cases
/// included, as the IR does before it looks for a `return`.
fn without_functions(body: &mut Vec<Stmt>) {
    body.retain(|stmt| !matches!(stmt.kind, StmtKind::Def(_)));
    for stmt in body.iter_mut() {
        match &mut stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, block) in branches.iter_mut() {
                    without_functions(block);
                }
                if let Some(block) = otherwise {
                    without_functions(block);
                }
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
                without_functions(body);
            }
            StmtKind::OnFailure { cleanup, body, .. } => {
                without_functions(cleanup);
                without_functions(body);
            }
            _ => {}
        }
    }
    visit::in_stmts(body, &mut |seen| {
        if let ExprKind::Match { arms, .. } = &mut seen.kind {
            for arm in arms.iter_mut() {
                without_functions(&mut arm.body);
            }
        }
    });
}

/// What a call's rounds made, in order, those that made something, and what stopped it, if
/// anything did.
pub(crate) struct Ran {
    made: Vec<Value>,
    stop: Option<Flow>,
    /// How many of `made` the rounds before the one that stopped the call made: those after it
    /// are rounds that may not run.
    before: usize,
}

/// The most positions a leaf of a reduction's tree holds, as the runtime's `REDUCE_LEAF`: set by
/// the list's length alone, so the tree is the same at any thread count.
const REDUCE_LEAF: usize = 1024;

/// What fails in a sum's tree: the round that stopped the call, or an add that overflowed.
enum TreeFailure {
    Round,
    Add(Flow),
}

/// The parts `from..to` of a lifted `sum`, added as the runtime's tree adds them: a range of at
/// most [`REDUCE_LEAF`] positions from its first part on, in order, and a longer one as its two
/// halves, split at the middle, the earlier first. The parts end at the round that stopped the
/// call, if one did. What fails comes with where its range starts, as the runtime passes up the
/// failure of the range that starts lowest; a range from past the round that stopped the call
/// starts higher than that round's own, so what it would do is never needed.
fn tree_sum(parts: &[Value], (from, to): (usize, usize), span: Span) -> Result<Value, (usize, TreeFailure)> {
    if to - from <= REDUCE_LEAF {
        let mut total = parts.get(from).cloned().ok_or((from, TreeFailure::Round))?;
        for at in from + 1..to {
            let part = parts.get(at).ok_or((from, TreeFailure::Round))?;
            total = added(total, part, span).map_err(|flow| (from, TreeFailure::Add(flow)))?;
        }
        return Ok(total);
    }
    let middle = from + (to - from) / 2;
    match (tree_sum(parts, (from, middle), span), tree_sum(parts, (middle, to), span)) {
        (Ok(left), Ok(right)) => added(left, &right, span).map_err(|flow| (from, TreeFailure::Add(flow))),
        (Err(left), Err(right)) => Err(if right.0 < left.0 { right } else { left }),
        (Err(failed), Ok(_)) | (Ok(_), Err(failed)) => Err(failed),
    }
}

/// Two parts of a sum added, as compiled code adds them: ints checked, floats as they are.
fn added(left: Value, right: &Value, span: Span) -> Run<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => match a.checked_add(*b) {
            Some(sum) => Ok(Value::Int(sum)),
            None => crash("this calculation overflows an int", span),
        },
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (other, _) => unsupported(format!("`sum` over `parallel` of a {}", other.kind())),
    }
}

/// Whether `expr` uses `?` anywhere in it, as the IR asks before lifting a sum.
fn tries(expr: &Expr) -> bool {
    let mut found = false;
    visit::in_expr(&mut expr.clone(), &mut |seen| {
        found |= matches!(seen.kind, ExprKind::Try { .. });
    });
    found
}

/// What runs one round of a call, given the round and whether it may not run.
type Each<'a> = dyn FnMut(&mut Interp, usize, bool) -> Run<Option<Value>> + 'a;

/// Takes in how a round ended, after those before it: what it made, or the first to stop the
/// call. A failure after a `break` is refused, as it would pass up a failure or not.
fn settle(ran: Run<Option<Value>>, made: &mut Vec<Value>, stop: &mut Option<Flow>) -> Run<()> {
    match ran {
        Ok(value) => made.extend(value),
        Err(flow @ (Flow::Unsupported(_) | Flow::Abandoned)) => return Err(flow),
        Err(Flow::Return(_)) => return unsupported("a `return` inside a `parallel` round"),
        Err(flow) => match stop {
            None => *stop = Some(flow),
            Some(Flow::Break) if !matches!(flow, Flow::Break | Flow::Continue) => {
                return unsupported("a `parallel` round that may run after a `break` failing");
            }
            Some(_) => {}
        },
    }
    Ok(())
}

impl Interp {
    /// `parallel(source)`, when that's what an expression is, with the `limit=` it may take.
    pub(crate) fn parallel_call(&self, expr: &'static Expr) -> Run<Option<Written>> {
        let ExprKind::Call { callee, args } = &expr.kind else { return Ok(None) };
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        if name != "parallel" || self.is_bound(name) || self.program.function_at(name, callee.span).is_some()
        {
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
            Value::List(items) => items.into_inner(),
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

    /// Notes reaching outside the program, refusing it inside a `parallel` round, where its order
    /// isn't promised. A launched task's is held in order beside its block by `actors.rs`.
    pub(crate) fn reach_out(&self, outside: Outside) -> Run<()> {
        let Ok(scopes) = self.shared.scopes.lock() else {
            return unsupported("a `parallel` call was lost");
        };
        if scopes.iter().any(|scope| matches!(scope, Scope::Rounds(_))) {
            return unsupported(format!(
                "{} inside a `parallel` round, whose order isn't promised",
                outside.what()
            ));
        }
        Ok(())
    }

    /// Whether this runs in a `parallel` round with other rounds beside it, which could use any
    /// channel it waits on: one made in the round too, since its ends are plain values another
    /// round can be sent. What a wait of no time finds there hangs on time.
    pub(crate) fn beside_other_rounds(&self) -> Run<bool> {
        let Ok(scopes) = self.shared.scopes.lock() else {
            return unsupported("a `parallel` call was lost");
        };
        Ok(scopes.iter().any(|scope| matches!(scope, Scope::Rounds(call) if call.count > 1)))
    }

    /// Refuses what a `parallel` round does with `channel`, made outside it, where what runs
    /// beside it in the compiled program could come first, or not, as time has it: anything at
    /// all in a round that may not run, or in a stream's item, which runs alongside its reader; a
    /// receive while rounds after it haven't run here, whose messages could be the next; and a
    /// close, which other rounds' sends could come before or after.
    pub(crate) fn channel_in_rounds(&self, channel: i64, used: ChannelUse) -> Run<()> {
        let Ok(scopes) = self.shared.scopes.lock() else {
            return unsupported("a `parallel` call was lost");
        };
        for scope in scopes.iter() {
            let Scope::Rounds(call) = scope else { continue };
            if channel >= call.first_channel {
                continue;
            }
            if call.maybe {
                return unsupported("a `parallel` round that may not run using a channel");
            }
            if call.streamed {
                return unsupported(
                    "a channel used in a `parallel` stream's item, which runs alongside its reader",
                );
            }
            if used == ChannelUse::Receive && call.round + 1 < call.count && !call.handed {
                return unsupported(
                    "a receive in a `parallel` round before the rounds after it have run, whose messages could come first",
                );
            }
            if used == ChannelUse::Close {
                return unsupported(
                    "closing a channel in a `parallel` round, which other rounds' sends could come before or after",
                );
            }
        }
        Ok(())
    }

    /// Notes a `lock` on the `shared` value `cell`, to change it or not, in everything running
    /// side by side now, refusing one whose outcome depends on the order they run in.
    pub(crate) fn note_lock(&self, cell: u64, writes: bool) -> Run<()> {
        // One sent on a channel isn't the round's own that made it: another may have taken it.
        let sent = self.shared.sent_cells.lock().is_ok_and(|sent| sent.contains(&cell));
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
            if cell >= rounds.first_cell && !sent {
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
            let Ok(mut locked) = rounds.locked.lock() else {
                return unsupported("a `parallel` call was lost");
            };
            let clashes =
                locked.iter().any(|&(held, by, wrote)| held == cell && by != round && (wrote || writes));
            if clashes {
                return unsupported(
                    "a `shared` value one `parallel` round changes and another reaches, in an order that isn't promised",
                );
            }
            locked.push((cell, round, writes));
        }
        Ok(())
    }

    fn push_call(&self, streamed: bool, count: usize) -> Run<()> {
        let call = Call {
            round: 0,
            maybe: false,
            streamed,
            locked: Arc::default(),
            started: 0,
            first_cell: 0,
            count,
            first_channel: 0,
            handed: false,
        };
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
        let first_channel = self.channels_made()?;
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
        call.first_channel = first_channel;
        Ok(())
    }

    /// Runs `each` for each of `count` rounds, in order, on this thread, until one waits on a
    /// channel with rounds after it: those are handed to a thread of their own, which runs them
    /// from `work`, as they run beside it in the compiled program, and what they made is taken in
    /// once they've ended. After one stops the call, by failing, crashing or breaking out, the
    /// rest run as rounds that may not run: their failures are passed over, since the first
    /// round's is the one passed up, and refused after a `break`, which would pass up a failure
    /// or not.
    fn rounds(&mut self, count: usize, work: Option<Arc<Work>>, each: &mut Each<'_>) -> Run<Ran> {
        let scope = self.shared.scopes.lock().map_or(0, |scopes| scopes.len());
        self.push_call(false, count)?;
        // Rounds are handed on only from a thread with nothing open of its own a copy can't hold.
        let own = self.yields.is_none()
            && self.write_backs.is_empty()
            && self.recordings.is_empty()
            && self.loadings.is_empty();
        let work = work.filter(|_| own);
        let (frames, mark, level) = (self.frames.len(), self.mark(), self.rounds_as.len());
        self.handing.push(Handing {
            work,
            count,
            round: 0,
            maybe: false,
            frames,
            mark,
            scope,
            level,
            handed: None,
        });
        let ran = self.each_round(0, count, each);
        self.handing.pop();
        self.pop_scope();
        let (ran, numbered) = ran?;
        self.rounds_ended(&numbered)?;
        Ok(ran)
    }

    /// Runs rounds `from` on of the innermost call this thread runs, until they're done or the
    /// rest are handed on, and then waits for those: what they all made, and the numbers of those
    /// that used a channel.
    pub(crate) fn each_round(&mut self, from: usize, count: usize, each: &mut Each<'_>) -> Run<RoundsRan> {
        let mut made = Vec::with_capacity(count.saturating_sub(from));
        let mut stop: Option<Flow> = None;
        let mut before = None;
        // The numbers of the rounds that used a channel, each one of its own to them.
        let mut numbered = Vec::new();
        for at in from..count {
            if self.handing.last().is_some_and(|handing| handing.handed.is_some()) {
                break;
            }
            let maybe = stop.is_some();
            self.start_round(at, maybe)?;
            if let Some(handing) = self.handing.last_mut() {
                handing.round = at;
                handing.maybe = maybe;
            }
            self.rounds_as.push(None);
            let ran = each(self, at, maybe);
            numbered.extend(self.rounds_as.pop().flatten());
            if let Err(flow) = settle(ran, &mut made, &mut stop) {
                self.give_up_handed();
                return Err(flow);
            }
            if stop.is_some() && before.is_none() {
                before = Some(made.len());
            }
        }
        if let Some(handed) = self.handing.last_mut().and_then(|handing| handing.handed.take()) {
            // The round that handed the rest on stopped the call, but the rounds after it ran
            // beside it, where in the compiled program they may not have run at all.
            if stop.is_some() {
                self.give_up();
                return unsupported(
                    "a `parallel` round that stops the call after the rounds after it ran beside it",
                );
            }
            let (rest, rest_numbered) = self.rest_ended(&handed)?;
            if rest.stop.is_some() {
                before = Some(made.len() + rest.before);
            }
            made.extend(rest.made);
            stop = rest.stop;
            numbered.extend(rest_numbered);
        }
        let before = before.unwrap_or(made.len());
        Ok((Ran { made, stop, before }, numbered))
    }

    /// Gives up the run where a call's rounds were handed on and this thread can't take in what
    /// they make: it's refused, and they're to end.
    fn give_up_handed(&self) {
        if self.handing.last().is_some_and(|handing| handing.handed.is_some()) {
            self.give_up();
        }
    }

    /// Round `at` of `work`.
    pub(crate) fn run_round(&mut self, work: &Work, at: usize) -> Run<Option<Value>> {
        match work {
            Work::For { names, body, items } => {
                let item = items.get(at).cloned().unwrap_or(Value::Unit);
                let mark = self.mark();
                let ran = self.bind_fresh(names, item).and_then(|()| self.stmts(body));
                self.restore(mark);
                match ran {
                    Ok(()) | Err(Flow::Continue) => Ok(None),
                    Err(other) => Err(other),
                }
            }
            Work::Made { elem, value, names, cond, items } => {
                let item = items.get(at).cloned().unwrap_or(Value::Unit);
                self.made_from((elem, *value), names, item, *cond)
            }
        }
    }

    /// `for names in parallel(source):`, each round with its item.
    pub(crate) fn for_parallel(
        &mut self,
        names: &'static [String],
        call: Written,
        body: &'static [Stmt],
    ) -> Run<()> {
        let items = self.parallel_items(call)?;
        let count = items.len();
        let work = Arc::new(Work::For { names, body, items });
        let ran =
            self.rounds(count, Some(Arc::clone(&work)), &mut |interp, at, _| interp.run_round(&work, at))?;
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
        let (positions, own): (Vec<Value>, bool) = match self.read_place(&place, span)? {
            Value::List(items) => ((0..items.len()).map(|at| Value::Int(as_int(at))).collect(), false),
            Value::Table(table) => (table.live().map(|(id, _)| Value::Id(id)).collect(), false),
            // A type with positions of its own splits by them, its empty ones left out, as
            // `for ref` goes over them.
            Value::Instance(instance) => {
                let mut live = Vec::new();
                for position in self.own_positions(&place, &instance.ty, span)? {
                    if self.live_at(&place, &instance.ty, &position, span)? {
                        live.push(position);
                    }
                }
                (live, true)
            }
            other => return unsupported(format!("`for ref` over `parallel` of a {}", other.kind())),
        };
        self.parallel_limit(call)?;
        let ran = self.rounds(positions.len(), None, &mut |interp, at, maybe| {
            let position = positions.get(at).cloned().unwrap_or(Value::Unit);
            let item = if own {
                match interp.indexed_place(place.clone(), position, false, span)? {
                    Some(item) => item,
                    None => return unsupported("a position its type's `def []` gives no place for"),
                }
            } else {
                place.then(Step::At(position))
            };
            interp.ref_round(&item, name, body, maybe, span).map(|()| None)
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

    /// What each round of `[elem for names in parallel(source) if cond]` makes, by position, or
    /// of `{elem: value for ...}`, each a pair.
    fn parallel_made(
        &mut self,
        (elem, value): (&'static Expr, Option<&'static Expr>),
        names: &'static [String],
        call: Written,
        cond: Option<&'static Expr>,
    ) -> Run<(Ran, usize)> {
        let items = self.parallel_items(call)?;
        let count = items.len();
        let work = Arc::new(Work::Made { elem, value, names, cond, items });
        let ran =
            self.rounds(count, Some(Arc::clone(&work)), &mut |interp, at, _| interp.run_round(&work, at))?;
        Ok((ran, count))
    }

    /// What `elem` makes of an item, with `value` after it as a pair when there's one, or `None`
    /// when `cond` leaves it out.
    fn made_from(
        &mut self,
        (elem, value): (&'static Expr, Option<&'static Expr>),
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
            if !keep {
                return Ok(None);
            }
            let key = self.eval(elem)?;
            match value {
                Some(value) => Ok(Some(Value::Tuple(Held::new(vec![key, self.eval(value)?])))),
                None => Ok(Some(key)),
            }
        });
        self.restore(mark);
        made
    }

    /// `[elem for names in parallel(source) if cond]`: the rounds' results, in the list's order;
    /// for `{elem: value for ...}`, each a pair.
    pub(crate) fn parallel_list(
        &mut self,
        made: (&'static Expr, Option<&'static Expr>),
        names: &'static [String],
        call: Written,
        cond: Option<&'static Expr>,
    ) -> Run<Vec<Value>> {
        let (ran, _) = self.parallel_made(made, names, call, cond)?;
        if let Some(stop) = ran.stop {
            return Err(stop);
        }
        let made = ran.made;
        if made.len() > super::MOST_ITEMS {
            return unsupported("ran too long: a collection too big");
        }
        Ok(made)
    }

    /// `sum(elem for names in parallel(source) if cond)`, with the start it may take.
    ///
    /// Where compiled code lifts it into ranges, as the IR's `parallel_value` does: an int or
    /// float `sum` over a list, of one name, with no `if` and no `?`, its parts add in the
    /// runtime's fixed tree (`tree_sum`), and the start is added to what that comes to. Any other
    /// runs in order, as a plain `sum` adds, up to the round that stopped the call, whose failure
    /// is passed up unless an add before it overflowed first.
    pub(crate) fn parallel_sum(
        &mut self,
        generator: &'static Expr,
        call: Written,
        start: Option<Value>,
        expr: &'static Expr,
    ) -> Run<Value> {
        let ExprKind::Comprehension { elem, names, cond, .. } = &generator.kind else {
            return unsupported("a generator that isn't one");
        };
        let span = expr.span;
        let float = match self.program.type_at(span) {
            Some(Type::Int) => Some(false),
            Some(Type::Float) => Some(true),
            _ => None,
        };
        let lifted = float.is_some()
            && cond.is_none()
            && names.len() == 1
            && call.limit.is_none()
            && matches!(self.program.type_at(call.source.span), Some(Type::List(_)))
            && !tries(elem);
        let (Ran { mut made, stop, before }, count) =
            self.parallel_made((elem, None), names, call, cond.as_deref())?;
        made.truncate(before);
        if !lifted {
            let total = self.summed(ArgVal::Value(None, Value::List(Held::new(made))), start, expr)?;
            return match stop {
                Some(stop) => Err(stop),
                None => Ok(total),
            };
        }
        let zero = if float == Some(true) { Value::Float(0.0) } else { Value::Int(0) };
        let start = start.unwrap_or(zero);
        // An empty list's sum is the start, with no tree to add it to.
        if count == 0 {
            return Ok(start);
        }
        match tree_sum(&made, (0, count), span) {
            Ok(tree) => added(start, &tree, span),
            Err((_, TreeFailure::Add(flow))) => Err(flow),
            Err((_, TreeFailure::Round)) => {
                Err(stop.unwrap_or_else(|| Flow::Unsupported("a `parallel` call was lost".to_owned())))
            }
        }
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
        let count = items.len();
        for (at, item) in items.into_iter().enumerate() {
            self.push_call(true, count)?;
            self.rounds_as.push(None);
            let made =
                self.start_round(at, false).and_then(|()| self.made_from((elem, None), names, item, cond));
            let numbered: Vec<usize> = self.rounds_as.pop().flatten().into_iter().collect();
            self.pop_scope();
            self.rounds_ended(&numbered)?;
            if let Some(made) = made? {
                self.yield_value(made)?;
            }
        }
        Ok(())
    }
}
