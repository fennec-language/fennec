//! Where each owned local is read for the last time: after an instruction that reads it, no way on
//! reads it again before it's written, so it's let go of there rather than when the function
//! leaves. A value read twice, like a `?`'s result, was held to the end otherwise, and each way out
//! after it let go of it: in a long function, every way out let go of every one before it.
//!
//! Only a local nothing else stands for is let go of this way: not one something borrows from,
//! takes from, or reads in its place, nor a lock's handle or a crash's cleanup, which a crash reads
//! where nothing here says so, nor anything in a generator, whose locals live across a pause.

use std::collections::{BTreeSet, HashMap};

use fennec_ir::{Function, InstKind, LocalId, Operand, Terminator};

use crate::analysis::visit;

/// The locals let go of after each instruction, by its block and place in it.
pub(crate) type Dying = HashMap<(usize, usize), Vec<LocalId>>;

/// The locals `eligible` allows that `function` reads for the last time at each instruction.
pub(crate) fn dying(function: &Function, eligible: &dyn Fn(usize) -> bool) -> Dying {
    let blocks = &function.blocks;
    let reads_of = |inst: &InstKind| {
        let (mut reads, mut writes) = (BTreeSet::new(), BTreeSet::new());
        visit(
            inst,
            |operand| {
                if let Operand::Local(local) = operand
                    && eligible(local.0)
                {
                    reads.insert(local.0);
                }
            },
            |local| {
                if eligible(local.0) {
                    writes.insert(local.0);
                }
            },
        );
        (reads, writes)
    };
    let end_reads = |end: &Terminator| -> BTreeSet<usize> {
        let operand = match end {
            Terminator::Branch { cond, .. } => Some(cond),
            Terminator::Return(value) => value.as_ref(),
            Terminator::NoMatch { value, .. } => Some(value),
            Terminator::Jump(_) | Terminator::Stop { .. } | Terminator::Unreachable => None,
        };
        match operand {
            Some(Operand::Local(local)) if eligible(local.0) => BTreeSet::from([local.0]),
            _ => BTreeSet::new(),
        }
    };
    // What each block reads before writing it, and what it writes.
    let mut gen_kill: Vec<(BTreeSet<usize>, BTreeSet<usize>)> = Vec::with_capacity(blocks.len());
    for block in blocks {
        let mut live = end_reads(&block.end);
        let mut written = BTreeSet::new();
        for inst in block.insts.iter().rev() {
            let (reads, writes) = reads_of(&inst.kind);
            for local in &writes {
                live.remove(local);
            }
            written.extend(writes);
            live.extend(reads);
        }
        gen_kill.push((live, written));
    }
    let mut before: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    for (at, block) in blocks.iter().enumerate() {
        for next in block.end.targets() {
            if let Some(theirs) = before.get_mut(next.0) {
                theirs.push(at);
            }
        }
    }
    // What's live as each block starts, worked out back from the ends.
    let mut live_in: Vec<BTreeSet<usize>> = gen_kill.iter().map(|(reads, _)| reads.clone()).collect();
    let mut waiting: BTreeSet<usize> = (0..blocks.len()).collect();
    while let Some(at) = waiting.pop_last() {
        let (Some(block), Some((reads, written))) = (blocks.get(at), gen_kill.get(at)) else { continue };
        let out = live_out(block, &live_in);
        let mut now: BTreeSet<usize> = out.difference(written).copied().collect();
        now.extend(reads.iter().copied());
        if live_in.get(at).is_some_and(|was| *was != now) {
            if let Some(was) = live_in.get_mut(at) {
                *was = now;
            }
            waiting.extend(before.get(at).into_iter().flatten().copied());
        }
    }
    let mut dying = Dying::new();
    for (at, block) in blocks.iter().enumerate() {
        let mut live = live_out(block, &live_in);
        live.extend(end_reads(&block.end));
        for (index, inst) in block.insts.iter().enumerate().rev() {
            let (reads, writes) = reads_of(&inst.kind);
            let last: Vec<LocalId> = reads
                .iter()
                .filter(|local| !live.contains(local) && !writes.contains(local))
                .map(|local| LocalId(*local))
                .collect();
            if !last.is_empty() {
                dying.insert((at, index), last);
            }
            for local in &writes {
                live.remove(local);
            }
            live.extend(reads);
        }
    }
    dying
}

/// What's live as `block` ends: what any block after it reads before writing.
fn live_out(block: &fennec_ir::Block, live_in: &[BTreeSet<usize>]) -> BTreeSet<usize> {
    block.end.targets().iter().filter_map(|next| live_in.get(next.0)).flatten().copied().collect()
}
