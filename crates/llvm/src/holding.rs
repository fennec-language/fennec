//! Which of a function's locals may hold a value where, so a way out lets go of only those.
//!
//! A local starts empty. Writing it may fill it, and [`Emit::let_go_of`] empties it, which
//! stores zero into its slot. That goes for a struct or an enum held in place as for a block: its
//! slot is every word of it. Where every way to a point last emptied a local, the local is
//! empty there, and letting go of it would only check that.
//!
//! Without this, a return and the failure exit `prop` let go of every local, so every slot was
//! read at the end of the function. After `mem2reg`, that's a phi for each local at each join
//! and at each of `prop`'s many ways in: a long `main` of short-lived values grew without
//! bound, and `opt` ran for a day on one (coverage-opt-hang in the notes).
//!
//! A local held at only a few places is short-lived. In a function with many of them, each
//! failure while one may hold lets go of it before going to `prop`, and `prop` leaves it. Others
//! go at `prop`, as before.
//!
//! A local this misses would be left, not freed twice. A build that checks proofs checks that
//! every local a return leaves is empty.

use std::collections::BTreeSet;

use fennec_ir::{InstKind, LocalId};

use crate::analysis::visit;
use crate::emit::Emit;

/// Held at no more than this many instructions, a local is let go of where a failure leaves,
/// rather than at `prop`.
const SHORT: usize = 8;

/// How many short-lived locals a function needs before failures let go of them where they leave.
/// With fewer, `prop`'s phis for them are few, and the blocks each way out would add only weigh
/// on what LLVM does with the hot code around them: written for a `main` with a handful, they
/// were enough for it to unswitch one loop fewer, and `generics` ran 2.4% more instructions.
const MANY_SHORT: usize = 100;

/// What's worked out for one function.
#[derive(Debug, Default)]
pub(crate) struct Holding {
    /// The locals that may hold something as each block starts, or `None` for a block nothing
    /// reaches.
    coming_in: Vec<Option<BTreeSet<usize>>>,
    /// The locals followed: the function owns them, and only its instructions fill them.
    tracked: Vec<bool>,
    /// The followed locals a failure lets go of where it leaves.
    short: Vec<bool>,
}

impl Holding {
    /// The locals that may hold something as block `at` starts, when it's reached.
    pub(crate) fn coming_in(&self, at: usize) -> BTreeSet<usize> {
        match self.coming_in.get(at) {
            Some(Some(set)) => set.clone(),
            // Nothing reaches it, so nothing runs there, but nothing is assumed of it either.
            _ => self
                .tracked
                .iter()
                .enumerate()
                .filter(|(_, tracked)| **tracked)
                .map(|(local, _)| local)
                .collect(),
        }
    }

    pub(crate) fn tracked(&self, local: usize) -> bool {
        self.tracked.get(local).copied().unwrap_or(false)
    }

    pub(crate) fn short(&self, local: usize) -> bool {
        self.short.get(local).copied().unwrap_or(false)
    }
}

/// The followed locals `inst` may fill.
pub(crate) fn filled(holding: &Holding, inst: &InstKind) -> Vec<usize> {
    let mut written = Vec::new();
    visit(inst, |_| {}, |local: LocalId| written.push(local.0));
    written.retain(|local| holding.tracked(*local));
    written
}

/// Steps over one instruction: what it writes may hold after, and what it lets go of doesn't.
pub(crate) fn step(
    emit: &Emit<'_, '_, '_>,
    holding: &Holding,
    place: (usize, usize),
    inst: &InstKind,
    set: &mut BTreeSet<usize>,
) {
    set.extend(filled(holding, inst));
    for local in emit.let_go_of(place, inst) {
        set.remove(&local.0);
    }
}

/// Works it out for the function `emit` is writing. `split` says whether a failure may let go
/// of short-lived locals before `prop`: not when `prop` first puts back or rolls back what a
/// local's blocks hold, which needs them still there.
pub(crate) fn holding(emit: &Emit<'_, '_, '_>, untracked: &BTreeSet<usize>, split: bool) -> Holding {
    let function = emit.function;
    let locals = function.locals.len();
    let tracked: Vec<bool> = (0..locals)
        .map(|at| {
            at >= function.params
                && !untracked.contains(&at)
                && (emit.owns(LocalId(at)) || emit.owns_inline(LocalId(at)))
        })
        .collect();
    let mut holding =
        Holding { coming_in: vec![None; function.blocks.len()], tracked, short: vec![false; locals] };
    let Some(first) = holding.coming_in.first_mut() else { return holding };
    *first = Some(BTreeSet::new());
    // Blocks to go over again, each once at a time, lowest first, which is mostly the order
    // they run in.
    let mut waiting: BTreeSet<usize> = BTreeSet::from([0]);
    while let Some(at) = waiting.pop_first() {
        let (Some(block), Some(Some(start))) = (function.blocks.get(at), holding.coming_in.get(at)) else {
            continue;
        };
        let mut set = start.clone();
        for (index, inst) in block.insts.iter().enumerate() {
            step(emit, &holding, (at, index), &inst.kind, &mut set);
        }
        for next in block.end.targets() {
            let Some(theirs) = holding.coming_in.get_mut(next.0) else { continue };
            match theirs {
                Some(known) if set.is_subset(known) => {}
                Some(known) => {
                    known.extend(set.iter().copied());
                    waiting.insert(next.0);
                }
                None => {
                    *theirs = Some(set.clone());
                    waiting.insert(next.0);
                }
            }
        }
    }
    if split {
        let mut places = vec![0_usize; locals];
        for (at, block) in function.blocks.iter().enumerate() {
            let Some(Some(start)) = holding.coming_in.get(at) else { continue };
            let mut set = start.clone();
            for (index, inst) in block.insts.iter().enumerate() {
                for local in set.iter().copied().chain(filled(&holding, &inst.kind)) {
                    if let Some(count) = places.get_mut(local) {
                        *count += 1;
                    }
                }
                step(emit, &holding, (at, index), &inst.kind, &mut set);
            }
        }
        holding.short =
            holding.tracked.iter().zip(&places).map(|(tracked, count)| *tracked && *count <= SHORT).collect();
        if holding.short.iter().filter(|short| **short).count() < MANY_SHORT {
            holding.short = vec![false; locals];
        }
    }
    holding
}
