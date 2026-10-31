//! What a function's IR says about its ints, for any backend or the optimizer to use. It leaves
//! out checks that can't fail, and depends on nothing but the IR.
//!
//! A local written once holds one value for as long as anything can read it, and so does a
//! parameter nothing writes. What's known about such a local is what's known about that value,
//! narrowed by the branches that lead to where it's read. A `for` loop's count stays between
//! its first value and its limit wherever the loop's body reads it.

use std::collections::HashMap;

use fennec_syntax::BinOp;

use crate::{Block, Count, Function, InstKind, LocalId, Operand, Terminator};

/// The lowest and highest an int can be.
pub type Range = (i64, i64);

/// What a branch says about a local holding one value: `local op limit` came out as `holds`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Narrowed {
    pub local: LocalId,
    pub op: BinOp,
    pub limit: i64,
    pub holds: bool,
}

/// What's known about a function's locals.
#[derive(Clone, Debug)]
pub struct Facts {
    /// How many instructions write each local.
    writes: Vec<usize>,
    params: usize,
    /// What's known about the value of each local holding one, or a count's first value.
    ranges: HashMap<usize, Range>,
    /// The loop each count is the count of.
    counts: HashMap<usize, Count>,
    /// What branches say in each block.
    narrowed: Vec<Vec<Narrowed>>,
}

impl Facts {
    /// The facts of a function. `changes` says which arguments of a call to a function are `mut`.
    pub fn of(function: &Function, changes: &dyn Fn(&str) -> Vec<usize>) -> Self {
        Facts {
            writes: writes(function, changes),
            params: function.params,
            ranges: HashMap::new(),
            counts: function.counts.iter().map(|count| (count.count.0, *count)).collect(),
            narrowed: narrowings(function),
        }
    }

    /// Whether a local holds one value for as long as anything can read it.
    pub fn holds_one_value(&self, local: LocalId) -> bool {
        let written = self.writes.get(local.0).copied().unwrap_or(2);
        !self.counts.contains_key(&local.0) && written == usize::from(local.0 >= self.params)
    }

    /// Says what a parameter is known to hold, as when a function is inlined at a call.
    pub fn note_param(&mut self, local: LocalId, range: Range) {
        self.ranges.insert(local.0, range);
    }

    /// Says what's known about a value written to a local, which counts if that says what the
    /// local holds: the local holds one value, or this is a count's first value.
    pub fn note_write(&mut self, local: LocalId, range: Range) {
        let first_count = self.counts.contains_key(&local.0) && !self.ranges.contains_key(&local.0);
        if first_count || (local.0 >= self.params && self.holds_one_value(local)) {
            self.ranges.insert(local.0, range);
        }
    }

    /// What's known about a local where a block reads it.
    pub fn range_in(&self, local: LocalId, block: usize) -> Option<Range> {
        if let Some(count) = self.counts.get(&local.0) {
            let first = *self.ranges.get(&local.0)?;
            let stop = *self.ranges.get(&count.stop.0)?;
            return Some(if count.down {
                (stop.0.saturating_add(1).min(first.0), first.1)
            } else {
                (first.0, stop.1.saturating_sub(1).max(first.1))
            });
        }
        if !self.holds_one_value(local) {
            return None;
        }
        let mut known = self.ranges.get(&local.0).copied().unwrap_or((i64::MIN, i64::MAX));
        for fact in self.narrowed.get(block).into_iter().flatten().filter(|fact| fact.local == local) {
            known = narrowed_range(known, fact.op, fact.limit, fact.holds).unwrap_or(known);
        }
        Some(known)
    }
}

/// What's known about an int once `int op limit` has come out as `holds`, when that says
/// anything.
pub fn narrowed_range(old: Range, op: BinOp, limit: i64, holds: bool) -> Option<Range> {
    let op = if holds { Some(op) } else { negated(op) };
    Some(match op {
        Some(BinOp::Lt) => (old.0, old.1.min(limit.saturating_sub(1))),
        Some(BinOp::LtEq) => (old.0, old.1.min(limit)),
        Some(BinOp::Gt) => (old.0.max(limit.saturating_add(1)), old.1),
        Some(BinOp::GtEq) => (old.0.max(limit), old.1),
        Some(BinOp::Eq) => (limit, limit),
        _ => return None,
    })
}

/// The comparison that says the same with its sides swapped.
pub fn flipped(op: BinOp) -> Option<BinOp> {
    Some(match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::LtEq => BinOp::GtEq,
        BinOp::Gt => BinOp::Lt,
        BinOp::GtEq => BinOp::LtEq,
        BinOp::Eq => BinOp::Eq,
        BinOp::NotEq => BinOp::NotEq,
        _ => return None,
    })
}

/// The comparison that's true when this one is false.
fn negated(op: BinOp) -> Option<BinOp> {
    Some(match op {
        BinOp::Lt => BinOp::GtEq,
        BinOp::LtEq => BinOp::Gt,
        BinOp::Gt => BinOp::LtEq,
        BinOp::GtEq => BinOp::Lt,
        BinOp::NotEq => BinOp::Eq,
        _ => return None,
    })
}

/// How many instructions write each local, counting what changes one in place.
fn writes(function: &Function, changes: &dyn Fn(&str) -> Vec<usize>) -> Vec<usize> {
    let mut writes = vec![0; function.locals.len()];
    let mut count = |local: LocalId| {
        if let Some(written) = writes.get_mut(local.0) {
            *written += 1;
        }
    };
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        if let Some(dest) = inst.kind.dest() {
            count(dest);
        }
        match &inst.kind {
            InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } => count(*object),
            InstKind::Builtin { args, changes: true, .. } => {
                if let Some(Operand::Local(first)) = args.first() {
                    count(*first);
                }
            }
            InstKind::Call { function: callee, args, .. } => {
                for at in changes(callee) {
                    if let Some(Operand::Local(arg)) = args.get(at) {
                        count(*arg);
                    }
                }
            }
            _ => {}
        }
    }
    writes
}

/// What a block's branch says about one local, for the block it goes to when `holds`.
fn branch_fact(block: &Block, cond: &Operand, holds: bool) -> Option<Narrowed> {
    let Operand::Local(cond) = cond else { return None };
    let (op, lhs, rhs) = block.insts.iter().rev().find_map(|inst| match &inst.kind {
        InstKind::Binary { dest, op, lhs, rhs } if dest == cond => Some((*op, lhs, rhs)),
        _ => None,
    })?;
    match (lhs, rhs) {
        (Operand::Local(local), Operand::Int(limit)) => {
            Some(Narrowed { local: *local, op, limit: *limit, holds })
        }
        (Operand::Int(limit), Operand::Local(local)) => {
            Some(Narrowed { local: *local, op: flipped(op)?, limit: *limit, holds })
        }
        _ => None,
    }
}

/// What the branch from one block to another says, when it says anything.
fn fact_along(from: &Block, to: usize) -> Option<Narrowed> {
    match &from.end {
        Terminator::Branch { cond, then, otherwise } if then.0 == to && otherwise.0 != to => {
            branch_fact(from, cond, true)
        }
        Terminator::Branch { cond, then, otherwise } if otherwise.0 == to && then.0 != to => {
            branch_fact(from, cond, false)
        }
        _ => None,
    }
}

/// What branches say about each block: a block only one block leads to knows what that block's
/// branch came out as, and what's known in that block.
fn narrowings(function: &Function) -> Vec<Vec<Narrowed>> {
    let mut comes_from: Vec<Vec<usize>> = vec![Vec::new(); function.blocks.len()];
    for (at, block) in function.blocks.iter().enumerate() {
        for target in block.end.targets() {
            if let Some(from) = comes_from.get_mut(target.0) {
                from.push(at);
            }
        }
    }
    let only_way_in = |block: usize| match comes_from.get(block).map(Vec::as_slice) {
        Some([from]) => Some(*from),
        _ => None,
    };
    let mut known = Vec::with_capacity(function.blocks.len());
    for at in 0..function.blocks.len() {
        let mut facts = Vec::new();
        let (mut here, mut seen) = (at, vec![at]);
        while let Some(from) = only_way_in(here).filter(|from| !seen.contains(from)) {
            facts.extend(function.blocks.get(from).and_then(|block| fact_along(block, here)));
            seen.push(from);
            here = from;
        }
        known.push(facts);
    }
    known
}
