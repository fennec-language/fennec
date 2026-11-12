//! The edge of a `shared` value's lock, for a backend running on threads. Everything a `shared`
//! value holds is in its own table of blocks, and nothing outside points into it, so a value crossing
//! the lock's edge is copied: what the body reads from outside goes in, what it leaves outside comes
//! out, and what it only used stays behind, let go of while the lock's still held.
//!
//! The IR has no other way to say so, so this writes it in as three builtins:
//! `shared.in`, `shared.out` and `shared.drop`. A read lock copies nothing in: its body reads what's
//! outside as it is.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;

use crate::costs::insts::reads;
use crate::{Function, Inst, InstKind, Local, LocalId, Operand, Program, Terminator};

/// A lock's region in a function: what it holds, and the instructions from taking it to giving it
/// back, by block.
struct Region {
    block: usize,
    at: usize,
    write: bool,
    /// How many of its locks are held to write.
    writing: i64,
    handles: Vec<Operand>,
    /// For each block the region reaches, the range of its instructions in the region.
    ranges: HashMap<usize, (usize, usize)>,
    /// Where it's given back: a block, and that instruction's place in it.
    exits: Vec<(usize, usize)>,
}

fn gives_back(kind: &InstKind, handles: &[Operand]) -> bool {
    match kind {
        InstKind::Unlock { handle } => handles.first() == Some(handle),
        InstKind::UnlockAll { handles: all } => all.first() == handles.first(),
        _ => false,
    }
}

fn region_at(function: &Function, (block, at): (usize, usize), plain: &HashSet<String>) -> Option<Region> {
    let kind = &function.blocks.get(block)?.insts.get(at)?.kind;
    let (handles, writes): (Vec<Operand>, Vec<bool>) = match kind {
        InstKind::Lock { handle, write } => (vec![handle.clone()], vec![*write]),
        InstKind::LockAll { handles } => handles.iter().cloned().unzip(),
        _ => return None,
    };
    // A value holding only numbers has no table: nothing crosses the edge of its lock.
    let has_table = |handle: &Operand| match function.type_of(handle) {
        Some(Type::Named(name)) => !plain.contains(name.as_str()),
        _ => true,
    };
    if !handles.iter().any(has_table) {
        return None;
    }
    let writing =
        handles.iter().zip(&writes).map(|(handle, write)| i64::from(has_table(handle) && *write)).sum();
    let mut region =
        Region { block, at, write: writing > 0, writing, handles, ranges: HashMap::new(), exits: Vec::new() };
    let mut left = vec![(block, at + 1)];
    while let Some((current, start)) = left.pop() {
        if region.ranges.get(&current).is_some_and(|(from, _)| *from <= start) {
            continue;
        }
        let held = function.blocks.get(current)?;
        let exit = held
            .insts
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, inst)| gives_back(&inst.kind, &region.handles))
            .map(|(place, _)| place);
        region.ranges.insert(current, (start, exit.unwrap_or(held.insts.len())));
        match exit {
            Some(place) => region.exits.push((current, place)),
            None => left.extend(held.end.targets().iter().map(|target| (target.0, 0))),
        }
    }
    Some(region)
}

/// The locals an instruction changes, not counting what it's given by `mut`.
fn writes(inst: &InstKind, mut_params: &HashMap<&str, Vec<usize>>) -> Vec<LocalId> {
    let mut written: Vec<LocalId> = inst.dest().into_iter().collect();
    match inst {
        InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } => written.push(*object),
        InstKind::Builtin { args, changes: true, .. } => {
            if let Some(Operand::Local(local)) = args.first() {
                written.push(*local);
            }
        }
        InstKind::Call { function, args, .. } => {
            for at in mut_params.get(function.as_str()).into_iter().flatten() {
                if let Some(Operand::Local(local)) = args.get(*at) {
                    written.push(*local);
                }
            }
        }
        _ => {}
    }
    written
}

/// What's done at a region's edges, each local once, by what crosses.
#[derive(Default)]
struct Edge {
    /// Copied in, and given back as it was.
    read: Vec<LocalId>,
    /// Copied in if it's held by then, and copied out.
    changed: Vec<(LocalId, bool)>,
    /// Only used inside, and let go of there.
    inside: Vec<LocalId>,
}

fn edge_of(
    function: &Function,
    region: &Region,
    (handles, mut_params): (&HashSet<&str>, &HashMap<&str, Vec<usize>>),
) -> Edge {
    let crosses = |local: LocalId| {
        function.locals.get(local.0).is_some_and(|held| match &held.ty {
            Type::Int | Type::Float | Type::Bool | Type::None | Type::Unit => false,
            Type::Named(name) => !handles.contains(name.as_str()),
            _ => true,
        })
    };
    let (mut read, mut written, mut read_out, mut written_out) =
        (HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new());
    for (at, block) in function.blocks.iter().enumerate() {
        let range = region.ranges.get(&at);
        for (place, inst) in block.insts.iter().enumerate() {
            let inside = range.is_some_and(|(from, to)| *from <= place && place < *to);
            let (reading, writing) =
                if inside { (&mut read, &mut written) } else { (&mut read_out, &mut written_out) };
            reading.extend(reads(&inst.kind));
            writing.extend(writes(&inst.kind, mut_params));
        }
        if let Terminator::Branch { cond: Operand::Local(local), .. }
        | Terminator::Return(Some(Operand::Local(local))) = &block.end
        {
            read_out.insert(*local);
        }
    }
    let params = |local: LocalId| local.0 < function.params;
    let mut edge = Edge::default();
    let mut every: Vec<LocalId> =
        read.iter().chain(&written).copied().filter(|local| crosses(*local)).collect();
    every.sort_by_key(|local| local.0);
    every.dedup();
    for local in every {
        let elsewhere = params(local) || read_out.contains(&local) || written_out.contains(&local);
        let before = params(local) || written_out.contains(&local);
        if !elsewhere {
            edge.inside.push(local);
        } else if written.contains(&local) {
            edge.changed.push((local, before));
        } else {
            edge.read.push(local);
        }
    }
    edge
}

fn builtin(name: &str, dest: Option<LocalId>, local: LocalId, span: fennec_syntax::Span) -> Inst {
    let args = vec![Operand::Local(local)];
    Inst { kind: InstKind::Builtin { dest, name: name.to_owned(), args, changes: false }, span }
}

/// Writes in what crosses one region's edges.
fn guard(function: &mut Function, region: &Region, edge: &Edge) {
    let span = function
        .blocks
        .get(region.block)
        .and_then(|block| block.insts.get(region.at))
        .map(|inst| inst.span)
        .unwrap_or_default();
    let mut saved = Vec::new();
    for local in &edge.read {
        if region.write {
            let ty = function.locals.get(local.0).map_or(Type::Unit, |held| held.ty.clone());
            let keep = LocalId(function.locals.len());
            function.locals.push(Local { name: None, ty, copied: false });
            saved.push((*local, keep));
        }
    }
    // After the lock's taken: what's kept outside first, then what the body works on.
    let mut entering = Vec::new();
    for (local, keep) in &saved {
        entering.push(Inst { kind: InstKind::Copy { dest: *keep, value: Operand::Local(*local) }, span });
        entering.push(builtin("shared.in", Some(*local), *local, span));
    }
    for (local, before) in &edge.changed {
        if *before && region.write {
            entering.push(builtin("shared.in", Some(*local), *local, span));
        }
    }
    let mut leaving = Vec::new();
    for (local, _) in &saved {
        leaving.push(builtin("shared.drop", None, *local, span));
    }
    for (local, _) in &edge.changed {
        let args = vec![Operand::Local(*local), Operand::Int(region.writing)];
        let kind =
            InstKind::Builtin { dest: Some(*local), name: "shared.out".to_owned(), args, changes: false };
        leaving.push(Inst { kind, span });
    }
    for local in &edge.inside {
        leaving.push(builtin("shared.drop", None, *local, span));
    }
    let mut after = Vec::new();
    for (local, keep) in &saved {
        after.push(Inst { kind: InstKind::Copy { dest: *local, value: Operand::Local(*keep) }, span });
        after.push(builtin("shared.drop", None, *keep, span));
    }
    // From the end of each block back, so places found earlier stay right.
    let mut places: Vec<(usize, usize, Vec<Inst>)> = Vec::new();
    for (block, place) in &region.exits {
        places.push((*block, *place + 1, after.clone()));
        places.push((*block, *place, leaving.clone()));
    }
    places.push((region.block, region.at + 1, entering));
    places.sort_by_key(|(block, place, _)| std::cmp::Reverse((*block, *place)));
    for (block, place, insts) in places {
        if let Some(block) = function.blocks.get_mut(block) {
            block.insts.splice(place..place, insts);
        }
    }
}

/// `len(box.items)`, read under a lock of its own just for the field, needs no copy of the list if
/// the lock's held for the length too.
fn keep_lock_for_len(function: &mut Function) {
    let mut uses: HashMap<LocalId, usize> = HashMap::new();
    for block in &function.blocks {
        for inst in &block.insts {
            for local in reads(&inst.kind) {
                *uses.entry(local).or_default() += 1;
            }
        }
    }
    for block in &mut function.blocks {
        for at in 1..block.insts.len().saturating_sub(1) {
            let (Some(field), Some(lock), Some(next)) =
                (block.insts.get(at - 1), block.insts.get(at), block.insts.get(at + 1))
            else {
                continue;
            };
            let asked = matches!(&lock.kind, InstKind::Unlock { .. })
                && match (&field.kind, &next.kind) {
                    (InstKind::GetField { dest, .. }, InstKind::Builtin { name, args, .. }) => {
                        matches!(name.as_str(), "list.len" | "str.len" | "dict.len")
                            && args.as_slice() == [Operand::Local(*dest)]
                            && uses.get(dest) == Some(&1)
                    }
                    _ => false,
                };
            if asked {
                block.insts.swap(at, at + 1);
            }
        }
    }
}

/// Whether a type, in place, holds only numbers and bools.
fn only_numbers(ty: &Type, program: &Program) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool => true,
        Type::Named(name) => program.structs.iter().any(|def| {
            def.name == *name
                && !def.shared
                && def.fields.iter().all(|(_, field)| only_numbers(field, program))
        }),
        _ => false,
    }
}

/// Writes in what crosses the edge of every lock in a program whose `shared` values run on threads.
pub fn guard_shared(program: &mut Program) {
    let plain: HashSet<String> = program
        .structs
        .iter()
        .filter(|def| def.shared && def.fields.iter().all(|(_, field)| only_numbers(field, program)))
        .map(|def| def.name.clone())
        .collect();
    let handles: HashSet<String> =
        program.structs.iter().filter(|def| def.shared).map(|def| def.name.clone()).collect();
    let handles: HashSet<&str> = handles.iter().map(String::as_str).collect();
    let mut_params: HashMap<String, Vec<usize>> = program
        .functions
        .iter()
        .map(|function| (function.name.clone(), function.mut_params.clone()))
        .collect();
    let mut_params: HashMap<&str, Vec<usize>> =
        mut_params.iter().map(|(name, params)| (name.as_str(), params.clone())).collect();
    for function in &mut program.functions {
        keep_lock_for_len(function);
        let mut regions = Vec::new();
        for (block, held) in function.blocks.iter().enumerate() {
            for at in 0..held.insts.len() {
                regions.extend(region_at(function, (block, at), &plain));
            }
        }
        // The latest place first, so earlier regions' places stay right.
        regions.sort_by_key(|region| std::cmp::Reverse((region.block, region.at)));
        for region in regions {
            let edge = edge_of(function, &region, (&handles, &mut_params));
            guard(function, &region, &edge);
        }
    }
}
