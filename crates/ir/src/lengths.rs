//! What's known about the lengths of a program's lists, for a backend to leave out the checks
//! that an index is in range. It works over the whole program, as a backend's ranges of int
//! parameters do, and depends on nothing but the IR.
//!
//! A list parameter is known to be as long as a number, or as long as an earlier list parameter,
//! when every call gives it one and nothing in the function can change either length. So what's
//! known holds everywhere in the function's body. A function can also be known to give back a
//! list of a known length.
//!
//! Within a function, each list's length and each int's value is a sum: a number, plus whole
//! multiples of values known only by name, like a parameter's length or how many times a loop
//! has gone round. Two lists whose sums are the same are as long as each other. A loop that
//! changes a length by the same amount every time round has changed it by that amount times its
//! turns, and a `for` loop over a range goes round as many times as the range has numbers.
//!
//! What it can't follow, it doesn't guess. A length changed any way but by an `append`, a `sort`
//! or a `reverse`, or by a call that may change it, is a new unknown. A function something other
//! than a call by name may call, like `main`, a closure, a task or a generator, knows nothing of
//! its parameters.

use std::collections::{BTreeMap, HashMap, HashSet};

use fennec_check::Type;
use fennec_syntax::{BinOp, UnaryOp};

use crate::{Function, InstKind, LocalId, Operand, Program, Terminator};

/// How many rounds what's known is passed down calls through: a list made in `main` and handed
/// down five calls deep is known after five.
const ROUNDS: usize = 6;

/// How deeply loops nest before what an inner one changes is taken as unknown, rather than
/// followed round by round.
const DEEPEST_LOOP: usize = 6;

/// The changes to a list that keep its length.
const KEEPS_LENGTH: [&str; 2] = ["list.sort", "list.reverse"];

/// What's known about a list's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Length {
    /// As long as this many items.
    Fixed(i64),
    /// As long as the parameter at this place. For a parameter, that's an earlier one. For what
    /// a function gives back, it's as long as that parameter was when the function was called.
    Same(usize),
}

/// What's known about the lengths of a program's lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lengths {
    params: BTreeMap<String, Vec<Option<Length>>>,
    returns: BTreeMap<String, Length>,
}

impl Lengths {
    /// Works out what's known about a program's list lengths.
    pub fn of(program: &Program) -> Self {
        let functions: HashMap<&str, &Function> =
            every(program).map(|function| (function.name.as_str(), function)).collect();
        let kept = kept(program, &functions);
        let other_ways = called_other_ways(program);
        let mut known = Lengths::default();
        for _ in 0..ROUNDS {
            let world = World { functions: &functions, kept: &kept, known: &known };
            let mut calls: HashMap<&str, Vec<Vec<Option<Sum>>>> = HashMap::new();
            let mut next = Lengths::default();
            for function in every(program) {
                let walked = Walk::new(function, &world).run();
                for (callee, args) in walked.calls {
                    calls.entry(callee).or_default().push(args);
                }
                if let Some(length) = walked.returned {
                    next.returns.insert(function.name.clone(), length);
                }
            }
            for function in every(program) {
                let name = function.name.as_str();
                if other_ways.contains(name) {
                    continue;
                }
                let Some(calls) = calls.get(name) else { continue };
                let given = given(function, calls, kept.get(name).map_or(&[][..], Vec::as_slice));
                if given.iter().any(Option::is_some) {
                    next.params.insert(name.to_owned(), given);
                }
            }
            if next == known {
                break;
            }
            known = next;
        }
        known
    }

    /// What's known about a function's parameter, by its place, everywhere in its body.
    pub fn param(&self, function: &str, at: usize) -> Option<Length> {
        self.params.get(function)?.get(at).copied().flatten()
    }

    /// What's known about the length of the list a function gives back.
    pub fn returned(&self, function: &str) -> Option<Length> {
        self.returns.get(function).copied()
    }

    /// Every function with a parameter something is known about, with what's known of each.
    pub fn params(&self) -> impl Iterator<Item = (&str, &[Option<Length>])> {
        self.params.iter().map(|(name, known)| (name.as_str(), known.as_slice()))
    }
}

/// Every function of the program, a constant's included.
fn every(program: &Program) -> impl Iterator<Item = &Function> {
    program.functions.iter().chain(&program.constants)
}

/// Whether a local is a list, whose length is followed.
fn is_list(ty: &Type) -> bool {
    matches!(ty, Type::List(_))
}

/// The functions something other than a direct call may call, so whose parameters may be
/// anything: `main`, a closure's (a task's and a parallel loop's among them), a generator's, a
/// constant's, and one no call in the program names, like a test.
fn called_other_ways(program: &Program) -> HashSet<&str> {
    let mut other_ways: HashSet<&str> = HashSet::from(["main"]);
    let mut called = HashSet::new();
    for inst in every(program).flat_map(|function| &function.blocks).flat_map(|block| &block.insts) {
        match &inst.kind {
            InstKind::Call { function, .. } => {
                called.insert(function.as_str());
            }
            InstKind::MakeClosure { function, .. } => {
                other_ways.insert(function.as_str());
            }
            InstKind::MakeGenerator { generator, .. } => {
                other_ways.insert(generator.as_str());
            }
            InstKind::Resume { known: Some(name), .. } | InstKind::Constant { name, .. } => {
                other_ways.insert(name.as_str());
            }
            _ => {}
        }
    }
    for def in &program.generators {
        other_ways.insert(def.name.as_str());
        other_ways.insert(def.resume.as_str());
        other_ways.extend(def.abandon.as_deref());
    }
    for function in every(program).filter(|function| !called.contains(function.name.as_str())) {
        other_ways.insert(function.name.as_str());
    }
    other_ways
}

/// For each function, which of its parameters keep their length, from when it's called to when
/// it gives back: a list nothing in it can make longer or shorter. A parameter that isn't a list
/// keeps nothing.
fn kept<'p>(program: &'p Program, functions: &HashMap<&str, &Function>) -> HashMap<&'p str, Vec<bool>> {
    let mut kept: HashMap<&str, Vec<bool>> = every(program)
        .map(|function| {
            let lists = function.locals.iter().take(function.params).map(|local| is_list(&local.ty));
            (function.name.as_str(), lists.collect())
        })
        .collect();
    loop {
        let mut changed = false;
        for function in every(program) {
            for at in 0..function.params {
                let keeps = kept.get(function.name.as_str()).and_then(|params| params.get(at).copied());
                if keeps == Some(true) && !keeps_length(function, LocalId(at), functions, &kept) {
                    if let Some(keeps) =
                        kept.get_mut(function.name.as_str()).and_then(|params| params.get_mut(at))
                    {
                        *keeps = false;
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            return kept;
        }
    }
}

/// Whether nothing in a function can change a local list's length, given which parameters each
/// function keeps the length of.
fn keeps_length(
    function: &Function,
    list: LocalId,
    functions: &HashMap<&str, &Function>,
    kept: &HashMap<&str, Vec<bool>>,
) -> bool {
    let me = Operand::Local(list);
    function.blocks.iter().flat_map(|block| &block.insts).all(|inst| {
        if inst.kind.dest() == Some(list) {
            return false;
        }
        match &inst.kind {
            InstKind::Builtin { name, args, changes: true, .. } if args.first() == Some(&me) => {
                KEEPS_LENGTH.contains(&name.as_str())
            }
            InstKind::SetIndex { object, path, .. } => *object != list || path.is_empty(),
            InstKind::SetField { object, .. } => *object != list,
            InstKind::CallClosure { args, .. } => !args.contains(&me),
            InstKind::Call { function: callee, args, .. } => {
                args.iter().enumerate().filter(|(_, arg)| **arg == me).all(|(at, _)| {
                    let Some(callee) = functions.get(callee.as_str()) else { return false };
                    callee.params == args.len()
                        && (!callee.mut_params.contains(&at)
                            || kept.get(callee.name.as_str()).and_then(|params| params.get(at))
                                == Some(&true))
                })
            }
            _ => true,
        }
    })
}

/// What every call gives a function's list parameters: a number of items, or the length of an
/// earlier list parameter, as long as the function keeps both lengths.
fn given(function: &Function, calls: &[Vec<Option<Sum>>], kept: &[bool]) -> Vec<Option<Length>> {
    let params = function.params;
    if calls.iter().any(|args| args.len() != params) {
        return vec![None; params];
    }
    let keeps = |at: usize| kept.get(at).copied().unwrap_or(false);
    let at = |args: &[Option<Sum>], place: usize| args.get(place).cloned().flatten();
    (0..params)
        .map(|place| {
            if !keeps(place) {
                return None;
            }
            let lengths: Vec<Option<Sum>> = calls.iter().map(|args| at(args, place)).collect();
            if lengths.iter().any(Option::is_none) {
                return None;
            }
            let numbers: Vec<Option<i64>> =
                lengths.iter().map(|sum| sum.as_ref().and_then(Sum::as_number)).collect();
            if let Some(Some(first)) = numbers.first()
                && numbers.iter().all(|number| *number == Some(*first))
            {
                return Some(Length::Fixed(*first));
            }
            (0..place)
                .find(|&other| keeps(other) && calls.iter().all(|args| at(args, other) == at(args, place)))
                .map(Length::Same)
        })
        .collect()
}

/// What's known of every function while one is walked: their IR, which parameters each keeps
/// the length of, and what the last round found.
struct World<'w> {
    functions: &'w HashMap<&'w str, &'w Function>,
    kept: &'w HashMap<&'w str, Vec<bool>>,
    known: &'w Lengths,
}

/// A value known only by name, by number within one function's walk. A parameter's length or
/// value is named by the parameter's place.
type Name = usize;

/// A list's length or an int's value: a number, plus each name times how many of it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Sum {
    number: i64,
    names: BTreeMap<Name, i64>,
}

impl Sum {
    fn number(number: i64) -> Self {
        Sum { number, names: BTreeMap::new() }
    }

    fn name(name: Name) -> Self {
        Sum { number: 0, names: BTreeMap::from([(name, 1)]) }
    }

    fn as_number(&self) -> Option<i64> {
        self.names.is_empty().then_some(self.number)
    }

    /// This plus `times` of `other`, if no number in it overflows.
    fn plus(&self, other: &Sum, times: i64) -> Option<Sum> {
        let mut sum = self.clone();
        sum.number = sum.number.checked_add(other.number.checked_mul(times)?)?;
        for (name, count) in &other.names {
            let had = sum.names.get(name).copied().unwrap_or(0);
            let now = had.checked_add(count.checked_mul(times)?)?;
            if now == 0 {
                sum.names.remove(name);
            } else {
                sum.names.insert(*name, now);
            }
        }
        Some(sum)
    }

    /// This, with `value` for `name`.
    fn with(&self, name: Name, value: &Sum) -> Option<Sum> {
        let Some(&count) = self.names.get(&name) else { return Some(self.clone()) };
        self.plus(&Sum::name(name), -count)?.plus(value, count)
    }
}

/// What's known at one place in a function: the sum of each followed local, by its slot.
type State = Vec<Sum>;

/// A loop: its header, the blocks it goes round (the header first, in order), and the blocks
/// that go back to the header.
struct Loop {
    blocks: Vec<usize>,
    back: Vec<usize>,
}

/// What walking one function found.
struct Walked<'f> {
    /// Each direct call, with what's known of the length of each list it's given.
    calls: Vec<(&'f str, Vec<Option<Sum>>)>,
    /// What's known of the length of the list it gives back.
    returned: Option<Length>,
}

/// One function's walk: each block worked out in order, a loop's blocks twice to find how
/// much each length changes each time round.
struct Walk<'f, 'w> {
    function: &'f Function,
    world: &'w World<'w>,
    /// The slot of each followed local: the lists and ints.
    slots: Vec<Option<usize>>,
    /// Whether the local in each slot is a list.
    lists: Vec<bool>,
    /// Whether each name is known not to be negative: a length or a count of turns.
    not_negative: Vec<bool>,
    /// The name for a sum, or 0 when it's negative: how many times a loop goes round when its
    /// count may start past its limit. Two loops over the same range go round as many times.
    clamped: HashMap<Sum, Name>,
    preds: Vec<Vec<usize>>,
    /// Each loop, by its header.
    loops: BTreeMap<usize, Loop>,
    /// What's known at the end of each block.
    out: Vec<Option<State>>,
    /// The way out of a loop through its header's test, with how many times it went round.
    left: HashMap<(usize, usize), (Name, Sum)>,
    /// Whether calls and returns are noted: not while a loop is first walked to find its steps.
    noting: bool,
    calls: Vec<(&'f str, Vec<Option<Sum>>)>,
    returns: Vec<Option<Sum>>,
}

impl<'f, 'w> Walk<'f, 'w> {
    fn new(function: &'f Function, world: &'w World<'w>) -> Self {
        let counted = counted_ints(function);
        let mut slots = Vec::with_capacity(function.locals.len());
        let mut lists = Vec::new();
        for (at, local) in function.locals.iter().enumerate() {
            if is_list(&local.ty) || counted.contains(&at) {
                slots.push(Some(lists.len()));
                lists.push(is_list(&local.ty));
            } else {
                slots.push(None);
            }
        }
        let not_negative =
            function.locals.iter().take(function.params).map(|local| is_list(&local.ty)).collect();
        Walk {
            function,
            world,
            slots,
            lists,
            not_negative,
            clamped: HashMap::new(),
            preds: vec![Vec::new(); function.blocks.len()],
            loops: BTreeMap::new(),
            out: vec![None; function.blocks.len()],
            left: HashMap::new(),
            noting: true,
            calls: Vec::new(),
            returns: Vec::new(),
        }
    }

    /// A new name, known not to be negative when it's a length.
    fn fresh(&mut self, length: bool) -> Name {
        self.not_negative.push(length);
        self.not_negative.len() - 1
    }

    /// A new name for what a slot holds, which can't be negative when it's a list's length.
    fn unknown(&mut self, slot: usize) -> Sum {
        let length = self.lists.get(slot).copied().unwrap_or(false);
        Sum::name(self.fresh(length))
    }

    fn slot(&self, local: LocalId) -> Option<usize> {
        self.slots.get(local.0).copied().flatten()
    }

    fn preds_of(&self, block: usize) -> &[usize] {
        self.preds.get(block).map_or(&[][..], Vec::as_slice)
    }

    fn out_of(&self, block: usize) -> Option<&State> {
        self.out.get(block)?.as_ref()
    }

    fn set_out(&mut self, block: usize, state: State) {
        if let Some(place) = self.out.get_mut(block) {
            *place = Some(state);
        }
    }

    fn run(mut self) -> Walked<'f> {
        let Some(order) = self.order() else {
            // Control goes round some way that isn't a loop: nothing here is known, so every
            // call is given anything.
            let calls = self.function.blocks.iter().flat_map(|block| &block.insts).filter_map(|inst| {
                let InstKind::Call { function, args, .. } = &inst.kind else { return None };
                Some((function.as_str(), vec![None; args.len()]))
            });
            return Walked { calls: calls.collect(), returned: None };
        };
        let first = self.first_state();
        self.region(&order, &first, 0);
        let returned = self.returned();
        Walked { calls: self.calls, returned }
    }

    /// What's known on the way in: each list parameter's length, by its name, or by what every
    /// call gives it, and every other local unknown.
    fn first_state(&mut self) -> State {
        let mut state = Vec::with_capacity(self.lists.len());
        for (local, slot) in self.slots.clone().into_iter().enumerate() {
            let Some(slot) = slot else { continue };
            let sum = if local < self.function.params {
                match self.world.known.param(&self.function.name, local) {
                    Some(Length::Fixed(number)) => Sum::number(number),
                    Some(Length::Same(other)) => Sum::name(other),
                    None => Sum::name(local),
                }
            } else {
                self.unknown(slot)
            };
            state.push(sum);
        }
        state
    }

    /// The blocks in the order they're reached, each loop found on the way, or `None` when
    /// control goes back to a block that doesn't come before everything going round to it.
    fn order(&mut self) -> Option<Vec<usize>> {
        let count = self.function.blocks.len();
        let mut post = Vec::with_capacity(count);
        let mut seen = vec![false; count];
        *seen.get_mut(0)? = true;
        let mut stack = vec![(0, 0)];
        while let Some((block, next)) = stack.pop() {
            let targets = self.function.blocks.get(block)?.end.targets();
            if let Some(target) = targets.get(next) {
                stack.push((block, next + 1));
                let target = target.0;
                self.preds.get_mut(target)?.push(block);
                let reached = seen.get_mut(target)?;
                if !*reached {
                    *reached = true;
                    stack.push((target, 0));
                }
            } else {
                post.push(block);
            }
        }
        post.reverse();
        let mut place = vec![usize::MAX; count];
        for (at, block) in post.iter().enumerate() {
            *place.get_mut(*block)? = at;
        }
        let place_of = |block: usize| place.get(block).copied().unwrap_or(usize::MAX);
        let dominators = dominators(&post, &self.preds, &place);
        for &block in &post {
            for &pred in self.preds.get(block).into_iter().flatten() {
                if place_of(pred) < place_of(block) {
                    continue;
                }
                // Going back: a loop only if `block` comes before everything on the way round.
                if !dominates(&dominators, place_of(block), place_of(pred)) {
                    return None;
                }
                let found =
                    self.loops.entry(block).or_insert_with(|| Loop { blocks: Vec::new(), back: Vec::new() });
                found.back.push(pred);
            }
        }
        if self.loops.contains_key(&0) {
            // Nothing comes before the start to say what's known on the way into the loop.
            return None;
        }
        let preds = &self.preds;
        for (header, found) in &mut self.loops {
            let mut inside = vec![false; count];
            *inside.get_mut(*header)? = true;
            let mut work = found.back.clone();
            while let Some(block) = work.pop() {
                let reached = inside.get_mut(block)?;
                if !*reached {
                    *reached = true;
                    work.extend(preds.get(block).into_iter().flatten().copied());
                }
            }
            found.blocks = post.iter().copied().filter(|block| inside.get(*block) == Some(&true)).collect();
        }
        Some(post)
    }

    /// Works out the blocks of a region in order, the first with `first` on the way in. A loop
    /// inside it is worked out as a whole, from its header.
    fn region(&mut self, blocks: &[usize], first: &State, depth: usize) {
        let mut done: HashSet<usize> = HashSet::new();
        for (at, &block) in blocks.iter().enumerate() {
            if done.contains(&block) {
                continue;
            }
            if at == 0 {
                let out = self.block(block, first.clone());
                self.set_out(block, out);
                continue;
            }
            if let Some(found) = self.loops.get(&block) {
                let inner = found.blocks.clone();
                let entry = self.joined_in(block, true);
                done.extend(inner.iter().copied());
                self.go_round(block, &inner, &entry, depth + 1);
                continue;
            }
            let state = self.joined_in(block, false);
            let out = self.block(block, state);
            self.set_out(block, out);
        }
    }

    /// Works out a loop: first with every followed local named on the way into its header, to
    /// find how much each changes each time round, then with each as it was on the way in plus
    /// that much for each turn.
    fn go_round(&mut self, header: usize, blocks: &[usize], entry: &State, depth: usize) {
        let back = self.loops.get(&header).map(|found| found.back.clone()).unwrap_or_default();
        let steps: Vec<Option<i64>> = if depth > DEEPEST_LOOP {
            self.written(blocks).into_iter().map(|written| (!written).then_some(0)).collect()
        } else {
            let noting = self.noting;
            self.noting = false;
            let marks: Vec<Sum> = (0..entry.len()).map(|slot| self.unknown(slot)).collect();
            self.region(blocks, &marks, depth);
            self.noting = noting;
            marks
                .iter()
                .enumerate()
                .map(|(slot, mark)| {
                    let mut steps = back.iter().map(|block| {
                        let sum = self.out_of(*block)?.get(slot)?;
                        sum.plus(mark, -1)?.as_number()
                    });
                    let first = steps.next().flatten()?;
                    steps.all(|step| step == Some(first)).then_some(first)
                })
                .collect()
        };
        let turns = self.fresh(true);
        let mut state = Vec::with_capacity(entry.len());
        for (slot, (sum, step)) in entry.iter().zip(steps).enumerate() {
            let known = step.and_then(|step| sum.plus(&Sum::name(turns), step));
            state.push(known.unwrap_or_else(|| self.unknown(slot)));
        }
        self.region(blocks, &state, depth);
        let way_out = self.counted(header, blocks, turns);
        self.left.retain(|(from, _), _| *from != header);
        if let Some((out, value)) = way_out {
            self.left.insert((header, out), (turns, value));
        }
    }

    /// Which slots anything in these blocks may write.
    fn written(&self, blocks: &[usize]) -> Vec<bool> {
        let mut written = vec![false; self.lists.len()];
        let mut note = |local: LocalId| {
            if let Some(slot) = self.slot(local).and_then(|slot| written.get_mut(slot)) {
                *slot = true;
            }
        };
        let insts =
            blocks.iter().filter_map(|block| self.function.blocks.get(*block)).flat_map(|block| &block.insts);
        for inst in insts {
            if let Some(dest) = inst.kind.dest() {
                note(dest);
            }
            match &inst.kind {
                InstKind::Builtin { args, changes: true, .. } => {
                    if let Some(Operand::Local(local)) = args.first() {
                        note(*local);
                    }
                }
                InstKind::SetIndex { object, .. } | InstKind::SetField { object, .. } => note(*object),
                InstKind::Call { args, .. } | InstKind::CallClosure { args, .. } => {
                    for arg in args {
                        if let Operand::Local(local) = arg {
                            note(*local);
                        }
                    }
                }
                _ => {}
            }
        }
        written
    }

    /// How many times a loop went round when it leaves through its header's test, `count < stop`
    /// with the count stepping up by one each time round, or `count > stop` stepping down: the
    /// block it leaves to, and the number of turns.
    fn counted(&mut self, header: usize, blocks: &[usize], turns: Name) -> Option<(usize, Sum)> {
        let block = self.function.blocks.get(header)?;
        let state = self.out_of(header)?;
        let Terminator::Branch { cond: Operand::Local(cond), then, otherwise } = &block.end else {
            return None;
        };
        if !blocks.contains(&then.0) || blocks.contains(&otherwise.0) {
            return None;
        }
        let InstKind::Binary { dest, op, lhs, rhs } = &block.insts.last()?.kind else { return None };
        if dest != cond {
            return None;
        }
        let (count, limit) = (self.read(state, lhs)?, self.read(state, rhs)?);
        let step = match op {
            BinOp::Lt => 1,
            BinOp::Gt => -1,
            _ => return None,
        };
        if count.names.get(&turns) != Some(&step) || limit.names.contains_key(&turns) {
            return None;
        }
        let start = count.plus(&Sum::name(turns), -step)?;
        let gone = if step == 1 { limit.plus(&start, -1)? } else { start.plus(&limit, -1)? };
        if self.not_negative(&gone) {
            return Some((otherwise.0, gone));
        }
        let name = if let Some(name) = self.clamped.get(&gone) {
            *name
        } else {
            let name = self.fresh(true);
            self.clamped.insert(gone, name);
            name
        };
        Some((otherwise.0, Sum::name(name)))
    }

    /// Whether a sum can't be negative: a number that isn't, plus lengths and turns.
    fn not_negative(&self, sum: &Sum) -> bool {
        sum.number >= 0
            && sum.names.iter().all(|(name, count)| *count > 0 && self.not_negative.get(*name) == Some(&true))
    }

    /// What's known on the way into a block: the same as on every way in, or a name the ways in
    /// agree on. A loop's header leaves out the ways back round.
    fn joined_in(&mut self, block: usize, header: bool) -> State {
        let back = if header {
            self.loops.get(&block).map(|found| found.back.clone()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut ways: Vec<State> = Vec::new();
        for &pred in self.preds_of(block) {
            if back.contains(&pred) {
                continue;
            }
            let Some(out) = self.out_of(pred) else { continue };
            let mut state = out.clone();
            if let Some((turns, value)) = self.left.get(&(pred, block)) {
                for sum in &mut state {
                    // A sum too big to take the number of turns keeps their name.
                    *sum = sum.with(*turns, value).unwrap_or_else(|| sum.clone());
                }
            }
            ways.push(state);
        }
        let slots = self.lists.len();
        if ways.is_empty() {
            return (0..slots).map(|slot| self.unknown(slot)).collect();
        }
        // Slots whose sums differ the same way on each way in get one name between them.
        let mut joined: HashMap<Vec<Sum>, Name> = HashMap::new();
        (0..slots)
            .map(|slot| {
                let sums: Vec<Sum> = ways.iter().filter_map(|way| way.get(slot).cloned()).collect();
                if let [first, rest @ ..] = sums.as_slice()
                    && rest.iter().all(|sum| sum == first)
                {
                    return first.clone();
                }
                if let Some(name) = joined.get(&sums) {
                    return Sum::name(*name);
                }
                let length = self.lists.get(slot).copied().unwrap_or(false);
                let name = self.fresh(length);
                joined.insert(sums, name);
                Sum::name(name)
            })
            .collect()
    }

    /// The sum a value read has, if it's followed.
    fn read(&self, state: &State, operand: &Operand) -> Option<Sum> {
        match operand {
            Operand::Local(local) => state.get(self.slot(*local)?).cloned(),
            Operand::Int(number) => Some(Sum::number(*number)),
            _ => None,
        }
    }

    /// Gives a local a new value, if it's followed: `sum`, or a new unknown.
    fn write(&mut self, state: &mut State, local: LocalId, sum: Option<Sum>) {
        let Some(slot) = self.slot(local) else { return };
        let sum = match sum {
            Some(sum) => sum,
            None => self.unknown(slot),
        };
        if let Some(place) = state.get_mut(slot) {
            *place = sum;
        }
    }

    /// Works out one block, from what's known on the way in to what's known at its end.
    fn block(&mut self, block: usize, mut state: State) -> State {
        let function = self.function;
        let Some(found) = function.blocks.get(block) else { return state };
        for inst in &found.insts {
            self.step(&mut state, &inst.kind);
        }
        if self.noting
            && let Terminator::Return(Some(value)) = &found.end
            && is_list(&function.returns)
        {
            let sum = self.read(&state, value);
            self.returns.push(sum);
        }
        state
    }

    /// Works out one instruction.
    fn step(&mut self, state: &mut State, inst: &'f InstKind) {
        match inst {
            InstKind::MakeList { dest, items } | InstKind::ConstantList { dest, items } => {
                let length = i64::try_from(items.len()).ok().map(Sum::number);
                self.write(state, *dest, length);
            }
            InstKind::Copy { dest, value } => {
                let sum = self.read(state, value);
                self.write(state, *dest, sum);
            }
            InstKind::Binary { dest, op, lhs, rhs } => {
                let (lhs, rhs) = (self.read(state, lhs), self.read(state, rhs));
                let sum = match (op, lhs, rhs) {
                    (BinOp::Add, Some(lhs), Some(rhs)) => lhs.plus(&rhs, 1),
                    (BinOp::Sub, Some(lhs), Some(rhs)) => lhs.plus(&rhs, -1),
                    (BinOp::Mul, Some(lhs), Some(rhs)) => match (lhs.as_number(), rhs.as_number()) {
                        (Some(times), _) => Sum::number(0).plus(&rhs, times),
                        (_, Some(times)) => Sum::number(0).plus(&lhs, times),
                        _ => None,
                    },
                    _ => None,
                };
                self.write(state, *dest, sum);
            }
            InstKind::Unary { dest, op: UnaryOp::Neg, value } => {
                let sum = self.read(state, value).and_then(|value| Sum::number(0).plus(&value, -1));
                self.write(state, *dest, sum);
            }
            InstKind::Builtin { dest, name, args, changes } => {
                let length = match (name.as_str(), args.as_slice()) {
                    ("list.len", [list]) if self.is_list_operand(list) => self.read(state, list),
                    _ => None,
                };
                if *changes && let Some(Operand::Local(list)) = args.first() {
                    let sum = self.read(state, &Operand::Local(*list));
                    let now = match name.as_str() {
                        "list.append" => sum.and_then(|sum| sum.plus(&Sum::number(1), 1)),
                        kept if KEEPS_LENGTH.contains(&kept) => sum,
                        _ => None,
                    };
                    self.write(state, *list, now);
                }
                if let Some(dest) = dest {
                    self.write(state, *dest, length);
                }
            }
            InstKind::SetIndex { object, path, .. } => {
                if !path.is_empty() {
                    self.write(state, *object, None);
                }
            }
            InstKind::SetField { object, .. } => self.write(state, *object, None),
            InstKind::CallClosure { dest, args, .. } => {
                for arg in args {
                    if let Operand::Local(local) = arg {
                        self.write(state, *local, None);
                    }
                }
                if let Some(dest) = dest {
                    self.write(state, *dest, None);
                }
            }
            InstKind::Call { dest, function, args } => self.call(state, function, args, *dest),
            other => {
                if let Some(dest) = other.dest() {
                    self.write(state, dest, None);
                }
            }
        }
    }

    /// Whether a value read is a followed list.
    fn is_list_operand(&self, operand: &Operand) -> bool {
        let Operand::Local(local) = operand else { return false };
        self.slot(*local).is_some_and(|slot| self.lists.get(slot) == Some(&true))
    }

    /// Works out a direct call: notes what it gives each list, then what it changes, and what it
    /// gives back.
    fn call(&mut self, state: &mut State, callee: &'f str, args: &[Operand], dest: Option<LocalId>) {
        let given: Vec<Option<Sum>> = args
            .iter()
            .map(|arg| if self.is_list_operand(arg) { self.read(state, arg) } else { None })
            .collect();
        if self.noting {
            self.calls.push((callee, given.clone()));
        }
        let found = self.world.functions.get(callee).copied().filter(|found| found.params == args.len());
        for (at, arg) in args.iter().enumerate() {
            let Operand::Local(local) = arg else { continue };
            let keeps = match found {
                None => false,
                Some(found) if found.mut_params.contains(&at) => {
                    self.world.kept.get(callee).and_then(|params| params.get(at)) == Some(&true)
                }
                Some(_) => true,
            };
            if !keeps {
                self.write(state, *local, None);
            }
        }
        let Some(dest) = dest else { return };
        let length = match found.and(self.world.known.returned(callee)) {
            Some(Length::Fixed(number)) => Some(Sum::number(number)),
            Some(Length::Same(at)) => given.get(at).cloned().flatten(),
            None => None,
        };
        self.write(state, dest, length);
    }

    /// What's known of the length of the list the function gives back, the same at every return.
    fn returned(&self) -> Option<Length> {
        let [first, rest @ ..] = self.returns.as_slice() else { return None };
        let first = first.as_ref()?;
        if rest.iter().any(|sum| sum.as_ref() != Some(first)) {
            return None;
        }
        if let Some(number) = first.as_number() {
            return Some(Length::Fixed(number));
        }
        let [(&param, &1)] = first.names.iter().collect::<Vec<_>>()[..] else { return None };
        let list = self.function.locals.get(param).is_some_and(|local| is_list(&local.ty));
        (first.number == 0 && param < self.function.params && list).then_some(Length::Same(param))
    }
}

/// The ints worth following: those a loop's test compares, and what they're worked out from,
/// back to lengths, numbers and parameters. Following every int would cost more than it tells.
fn counted_ints(function: &Function) -> HashSet<usize> {
    let int_local = |operand: &Operand| match operand {
        Operand::Local(local) if function.locals.get(local.0).is_some_and(|local| local.ty == Type::Int) => {
            Some(local.0)
        }
        _ => None,
    };
    let insts = || function.blocks.iter().flat_map(|block| &block.insts);
    let mut counted: HashSet<usize> = HashSet::new();
    for count in &function.counts {
        counted.extend([count.count.0, count.stop.0]);
    }
    for inst in insts() {
        if let InstKind::Binary { op: BinOp::Lt | BinOp::Gt, lhs, rhs, .. } = &inst.kind {
            counted.extend(int_local(lhs).into_iter().chain(int_local(rhs)));
        }
    }
    loop {
        let before = counted.len();
        for inst in insts() {
            let Some(dest) = inst.kind.dest() else { continue };
            if !counted.contains(&dest.0) {
                continue;
            }
            match &inst.kind {
                InstKind::Copy { value, .. } | InstKind::Unary { value, .. } => {
                    counted.extend(int_local(value));
                }
                InstKind::Binary { lhs, rhs, .. } => {
                    counted.extend(int_local(lhs).into_iter().chain(int_local(rhs)));
                }
                _ => {}
            }
        }
        if counted.len() == before {
            return counted;
        }
    }
}

/// Each reachable block's immediate dominator, by places in `order`, where control enters at the
/// first.
fn dominators(order: &[usize], preds: &[Vec<usize>], place: &[usize]) -> Vec<usize> {
    let mut idom = vec![usize::MAX; order.len()];
    if let Some(start) = idom.first_mut() {
        *start = 0;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (at, &block) in order.iter().enumerate().skip(1) {
            let mut found = usize::MAX;
            for &from in preds.get(block).into_iter().flatten() {
                let from = place.get(from).copied().unwrap_or(usize::MAX);
                if idom.get(from).is_none_or(|known| *known == usize::MAX) {
                    continue;
                }
                found = if found == usize::MAX { from } else { meet(&idom, from, found) };
            }
            if let Some(known) = idom.get_mut(at)
                && *known != found
            {
                *known = found;
                changed = true;
            }
        }
    }
    idom
}

/// Where two blocks' chains of dominators first meet, by place.
fn meet(idom: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while a > b {
            a = idom.get(a).copied().unwrap_or(0);
        }
        while b > a {
            b = idom.get(b).copied().unwrap_or(0);
        }
    }
    a
}

/// Whether every way from the start to the block at place `at` goes through the one at `over`.
fn dominates(idom: &[usize], over: usize, mut at: usize) -> bool {
    loop {
        if at == over {
            return true;
        }
        if at == 0 {
            return false;
        }
        match idom.get(at) {
            Some(&next) if next < at => at = next,
            _ => return false,
        }
    }
}
