//! A `list[str]` that's only ever added to and then joined with `""` is kept as the string it
//! becomes: each piece is copied onto its end, in place while there's room, and the join is the
//! string itself. Nothing else may look at it, and a function adding to one it's given gets one
//! from every call. So the pieces, and the list holding them, never become blocks of their own.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_ir::{Function, InstKind, Operand, Program, Terminator};

use crate::analysis::visit;

fn is_text(function: &Function, operand: &Operand) -> bool {
    match operand {
        Operand::Str(_) => true,
        Operand::Local(local) => function.locals.get(local.0).is_some_and(|held| held.ty == Type::Str),
        _ => false,
    }
}

/// For each function, the locals kept as the string they're joined into.
pub(crate) fn builders<'p>(
    program: &'p Program,
    functions: &HashMap<&'p str, &'p Function>,
) -> HashMap<&'p str, HashSet<usize>> {
    let mut kept: HashMap<&str, HashSet<usize>> = functions
        .iter()
        .map(|(name, function)| {
            let pieces = function
                .locals
                .iter()
                .enumerate()
                .filter(|(_, local)| local.ty == Type::List(Box::new(Type::Str)))
                .map(|(at, _)| at)
                .collect();
            (*name, pieces)
        })
        .collect();
    // A parameter needs every call to give it one, and only a call by name can.
    let mut other_ways: HashSet<&str> = HashSet::from(["main"]);
    for def in &program.generators {
        other_ways.insert(def.name.as_str());
        other_ways.insert(def.resume.as_str());
        other_ways.extend(def.abandon.as_deref());
    }
    for function in &program.constants {
        other_ways.insert(function.name.as_str());
    }
    for function in functions.values() {
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            if let InstKind::MakeClosure { function: made, .. } = &inst.kind {
                other_ways.insert(made.as_str());
            }
        }
    }
    loop {
        let mut dropped: Vec<(&str, usize)> = Vec::new();
        for (name, function) in functions {
            let mine = kept.get(name).cloned().unwrap_or_default();
            if mine.is_empty() {
                continue;
            }
            let is_kept = |local: usize| mine.contains(&local);
            for &param in mine.iter().filter(|at| **at < function.params) {
                if !function.mut_params.contains(&param) || other_ways.contains(name) {
                    dropped.push((name, param));
                }
            }
            for inst in function.blocks.iter().flat_map(|block| &block.insts) {
                let allowed: Vec<usize> = match &inst.kind {
                    InstKind::MakeList { dest, items }
                        if items.iter().all(|item| is_text(function, item)) =>
                    {
                        vec![dest.0]
                    }
                    InstKind::Copy { dest, value: Operand::Local(from) }
                        if is_kept(dest.0) && is_kept(from.0) =>
                    {
                        vec![dest.0, from.0]
                    }
                    InstKind::Builtin { name: builtin, args, .. } => {
                        match (builtin.as_str(), args.as_slice()) {
                            ("list.append", [Operand::Local(list), piece]) if is_text(function, piece) => {
                                vec![list.0]
                            }
                            ("str.join", [Operand::Str(between), Operand::Local(list)])
                                if between.is_empty() =>
                            {
                                vec![list.0]
                            }
                            _ => Vec::new(),
                        }
                    }
                    InstKind::Call { function: callee, args, .. } => {
                        let theirs = kept.get(callee.as_str());
                        let target = functions.get(callee.as_str());
                        args.iter()
                            .enumerate()
                            .filter_map(|(at, arg)| match arg {
                                Operand::Local(local)
                                    if target.is_some_and(|target| target.mut_params.contains(&at))
                                        && theirs.is_some_and(|params| params.contains(&at))
                                        && args.iter().filter(|other| *other == arg).count() == 1 =>
                                {
                                    Some(local.0)
                                }
                                _ => None,
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                };
                let (mut read, mut written) = (Vec::new(), Vec::new());
                visit(
                    &inst.kind,
                    |operand| {
                        if let Operand::Local(local) = operand {
                            read.push(local.0);
                        }
                    },
                    |local| written.push(local.0),
                );
                for local in read.into_iter().chain(written) {
                    if is_kept(local) && !allowed.contains(&local) {
                        dropped.push((name, local));
                    }
                }
                // What's given one at a parameter that takes one must give one.
                if let InstKind::Call { function: callee, args, .. } = &inst.kind
                    && let Some(theirs) = kept.get(callee.as_str())
                {
                    for &param in theirs {
                        let gives =
                            matches!(args.get(param), Some(Operand::Local(local)) if is_kept(local.0));
                        if !gives {
                            dropped.push((callee.as_str(), param));
                        }
                    }
                }
            }
            for block in &function.blocks {
                if let Terminator::Return(Some(Operand::Local(local))) = &block.end
                    && is_kept(local.0)
                {
                    dropped.push((name, local.0));
                }
            }
        }
        let mut changed = false;
        for (name, local) in dropped {
            if let Some(locals) = kept.get_mut(name) {
                changed |= locals.remove(&local);
            }
        }
        if !changed {
            return kept;
        }
    }
}
