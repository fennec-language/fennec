//! How many loops each block is in, and which functions are called from inside a loop.

use std::collections::{HashMap, HashSet};

use crate::{BlockId, Function, InstKind, Program};

/// How many loops each block of a function is in. A loop is a jump back to a block that every
/// path to the jump goes through, along with the blocks that can reach that jump without going
/// through that block.
pub(super) fn depths(function: &Function) -> Vec<usize> {
    let loops = bodies(function);
    (0..function.blocks.len()).map(|at| loops.values().filter(|body| body.contains(&at)).count()).collect()
}

/// Each loop of a function, by the block it jumps back to, with the blocks in it.
pub(super) fn bodies(function: &Function) -> HashMap<usize, HashSet<usize>> {
    let count = function.blocks.len();
    let successors: Vec<Vec<usize>> = function
        .blocks
        .iter()
        .map(|block| block.end.targets().into_iter().map(|BlockId(at)| at).collect())
        .collect();
    let mut predecessors = vec![Vec::new(); count];
    for (from, targets) in successors.iter().enumerate() {
        for to in targets {
            if let Some(list) = predecessors.get_mut(*to) {
                list.push(from);
            }
        }
    }
    let dominators = dominators(count, &predecessors);
    let mut loops: HashMap<usize, HashSet<usize>> = HashMap::new();
    for (from, targets) in successors.iter().enumerate() {
        for &head in targets {
            if dominators.get(from).is_some_and(|set| set.contains(&head)) {
                let body = loops.entry(head).or_insert_with(|| HashSet::from([head]));
                let mut pending = vec![from];
                while let Some(at) = pending.pop() {
                    if body.insert(at) {
                        pending.extend(predecessors.get(at).into_iter().flatten());
                    }
                }
            }
        }
    }
    loops
}

/// The blocks that every path from the first block to each block goes through.
fn dominators(count: usize, predecessors: &[Vec<usize>]) -> Vec<HashSet<usize>> {
    let every: HashSet<usize> = (0..count).collect();
    let mut found = vec![every; count];
    if let Some(first) = found.first_mut() {
        *first = HashSet::from([0]);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for at in 1..count {
            let mut through = predecessors.get(at).into_iter().flatten().filter_map(|from| found.get(*from));
            let mut set = through.next().cloned().unwrap_or_default();
            for other in through {
                set.retain(|block| other.contains(block));
            }
            set.insert(at);
            if found.get(at) != Some(&set) {
                changed = true;
                if let Some(slot) = found.get_mut(at) {
                    *slot = set;
                }
            }
        }
    }
    found
}

/// Each function that some call chain reaches from inside a loop, with the function whose loop
/// it is. Making a closure counts as calling it, since whatever it's given to may.
pub(super) fn callers_in_loops(program: &Program) -> HashMap<&str, &str> {
    let mut found: HashMap<&str, &str> = HashMap::new();
    let mut calls: Vec<(&str, &str)> = Vec::new();
    for function in &program.functions {
        let depth = depths(function);
        for (block, depth) in function.blocks.iter().zip(depth) {
            for inst in &block.insts {
                let (InstKind::Call { function: callee, .. }
                | InstKind::MakeClosure { function: callee, .. }) = &inst.kind
                else {
                    continue;
                };
                if depth > 0 {
                    found.entry(callee).or_insert(&function.name);
                }
                calls.push((&function.name, callee));
            }
        }
    }
    loop {
        let more: Vec<(&str, &str)> = calls
            .iter()
            .filter(|(caller, callee)| !found.contains_key(callee) && found.contains_key(caller))
            .filter_map(|(caller, callee)| Some((*callee, *found.get(caller)?)))
            .collect();
        if more.is_empty() {
            return found;
        }
        found.extend(more);
    }
}
