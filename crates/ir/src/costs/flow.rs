//! Where changing a value may copy it. Copying a collection only bumps a count, and the buffer
//! is copied when one of its holders changes it while another may still read it. So this
//! follows who holds what through a function, and at each change looks for a holder that's
//! read afterwards.
//!
//! A value changed once has its own buffer, so it isn't reported again until it's shared again.
//! What a call gives back is taken to share nothing with what it's passed.

use std::collections::{BTreeSet, HashMap, HashSet};

use fennec_check::Type;
use fennec_syntax::Span;

use super::insts::{dest, local, operands, reads, terminator_reads};
use super::{Cost, Shared as Why, on_the_heap};
use crate::{Function, InstKind, LocalId, Operand};

/// A value, by the local it's in and the fields and items to go through to reach it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    local: usize,
    path: Vec<String>,
}

/// One local holding a value that's also somewhere else, and whether it holds it inside, as an
/// item or a field, rather than as another name for the same value.
type Shared = (usize, Place, bool);
type Held = BTreeSet<Shared>;

/// How a list's item is written in a path, whichever item it is.
const ITEM: &str = "[]";

/// What holds nothing but a value being passed through: where a temporary read from a place
/// came from. Changing it changes that place, once it's written back.
fn places_of_temporaries(function: &Function) -> HashMap<usize, Place> {
    let mut set_times: HashMap<usize, usize> = HashMap::new();
    let insts = || function.blocks.iter().flat_map(|block| &block.insts);
    for inst in insts() {
        if let Some(id) = dest(&inst.kind) {
            *set_times.entry(id.0).or_default() += 1;
        }
    }
    let temporary = |id: usize| {
        id >= function.params
            && set_times.get(&id) == Some(&1)
            && function.locals.get(id).is_some_and(|local| local.name.is_none() && !local.copied)
    };
    let mut places: HashMap<usize, Place> = HashMap::new();
    for inst in insts() {
        let (id, from, step) = match &inst.kind {
            InstKind::Copy { dest, value } => (dest.0, value, None),
            InstKind::GetField { dest, object, field } => (dest.0, object, Some(field.clone())),
            InstKind::GetIndex { dest, object, .. } => (dest.0, object, Some(ITEM.to_owned())),
            _ => continue,
        };
        let Some(from) = local(from).filter(|_| temporary(id)) else { continue };
        let mut place = places.get(&from.0).cloned().unwrap_or(Place { local: from.0, path: Vec::new() });
        place.path.extend(step);
        places.insert(id, place);
    }
    places
}

/// Which locals are read after each instruction of each block, and after the block.
fn live_after(function: &Function) -> Vec<Vec<HashSet<usize>>> {
    let count = function.blocks.len();
    let mut live_in: Vec<HashSet<usize>> = vec![HashSet::new(); count];
    let mut after: Vec<Vec<HashSet<usize>>> = vec![Vec::new(); count];
    let mut changed = true;
    while changed {
        changed = false;
        for (at, block) in function.blocks.iter().enumerate().rev() {
            let targets = block.end.targets();
            let mut live: HashSet<usize> =
                targets.iter().filter_map(|target| live_in.get(target.0)).flatten().copied().collect();
            live.extend(terminator_reads(&block.end).iter().map(|id| id.0));
            let mut each = vec![HashSet::new(); block.insts.len()];
            for (inst, slot) in block.insts.iter().zip(&mut each).rev() {
                slot.clone_from(&live);
                if let Some(id) = dest(&inst.kind) {
                    live.remove(&id.0);
                }
                live.extend(reads(&inst.kind).iter().map(|id| id.0));
            }
            if live_in.get(at) != Some(&live) {
                changed = true;
                if let Some(slot) = live_in.get_mut(at) {
                    *slot = live;
                }
            }
            if let Some(slot) = after.get_mut(at) {
                *slot = each;
            }
        }
    }
    after
}

/// One function's walk through its blocks.
struct Flow<'f> {
    function: &'f Function,
    places: HashMap<usize, Place>,
    live: Vec<Vec<HashSet<usize>>>,
    /// The parameters that aren't `mut`, which the caller goes on holding.
    callers: HashSet<usize>,
    by_name: &'f HashMap<&'f str, &'f Function>,
}

impl Flow<'_> {
    fn is_heap(&self, id: usize) -> bool {
        self.function.locals.get(id).is_some_and(|local| holds_heap(&local.ty))
    }

    fn place(&self, id: usize) -> Place {
        self.places.get(&id).cloned().unwrap_or(Place { local: id, path: Vec::new() })
    }

    /// How a place reads in a message, by the names it goes through.
    fn shown(&self, place: &Place) -> String {
        let name = self.function.locals.get(place.local).and_then(|local| local.name.as_deref());
        let mut shown = name.unwrap_or("a value").to_owned();
        for step in place.path.iter().filter(|step| *step != ITEM) {
            shown.push('.');
            shown.push_str(step);
        }
        shown
    }

    /// Who may still read `place` once it's changed, and why it still holds it. `at` is the
    /// block and instruction doing the change.
    fn still_shared(
        &self,
        held: &Held,
        place: &Place,
        live: &HashSet<usize>,
        at: (usize, usize),
    ) -> Option<Why> {
        let alive = |id: usize| self.callers.contains(&id) || live.contains(&id);
        // Changing what's held copies it while its holder may still read it. Changing a holder
        // copies only what it shares as another name: changing a list doesn't copy its items.
        let (holder, inside) = held.iter().find_map(|(holder, value, inside)| {
            if value.local == place.local
                && place.path.starts_with(&value.path)
                && *holder != place.local
                && alive(*holder)
            {
                return Some((*holder, *inside));
            }
            (*holder == place.local && !inside && value.local != place.local && alive(value.local))
                .then_some((value.local, false))
        })?;
        if self.callers.contains(&holder) {
            return Some(Why::Caller);
        }
        let name = self.shown(&Place { local: holder, path: Vec::new() });
        let later = self.next_read(holder, at);
        if inside {
            return Some(Why::PutInto { container: name, later });
        }
        Some(Why::HeldBy { name, later })
    }

    /// Where `id` is next read after instruction `at`, following the blocks that come after.
    fn next_read(&self, id: usize, (block, inst): (usize, usize)) -> Option<Span> {
        let reads_it = |kind: &InstKind| reads(kind).iter().any(|read| read.0 == id);
        let here = self.function.blocks.get(block)?;
        if let Some(found) = here.insts.iter().skip(inst + 1).find(|candidate| reads_it(&candidate.kind)) {
            return Some(found.span);
        }
        let mut seen = HashSet::from([block]);
        let mut next: Vec<usize> = here.end.targets().iter().map(|target| target.0).collect();
        while let Some(at) = next.pop() {
            if !seen.insert(at) {
                continue;
            }
            let Some(found) = self.function.blocks.get(at) else { continue };
            if let Some(found) = found.insts.iter().find(|candidate| reads_it(&candidate.kind)) {
                return Some(found.span);
            }
            next.extend(found.end.targets().iter().map(|target| target.0));
        }
        None
    }

    /// Follows one instruction: what it changes, then what it makes shared.
    fn step(
        &self,
        held: &mut Held,
        kind: &InstKind,
        (live, at): (&HashSet<usize>, (usize, usize)),
    ) -> Option<Cost> {
        let mut cost = None;
        for written in self.written(kind) {
            let place = self.place(written.0);
            if let Some(why) = self.still_shared(held, &place, live, at) {
                cost = Some(Cost::Copy { value: self.shown(&place), why });
            }
            // Whichever holder changes it, the two don't share it any more. What's held from
            // elsewhere in the same value, like another field, stays shared.
            held.retain(|(holder, value, _)| {
                let changed = value.local == place.local && place.path.starts_with(&value.path);
                let holds_it = *holder == place.local && place.path.is_empty();
                !changed && !holds_it
            });
        }
        if let Some(id) = dest(kind) {
            held.retain(|(holder, value, _)| *holder != id.0 && value.local != id.0);
        }
        for (holder, value, inside) in self.shares(kind) {
            self.share(held, (holder, value), inside);
        }
        cost
    }

    /// The locals an instruction changes in place, buffers and all. Writing a whole field
    /// replaces what's there, so it copies nothing.
    fn written(&self, kind: &InstKind) -> Vec<LocalId> {
        match kind {
            InstKind::SetIndex { object, .. } => vec![*object],
            InstKind::Builtin { args, changes: true, .. } => {
                args.first().and_then(local).into_iter().filter(|id| self.is_heap(id.0)).collect()
            }
            InstKind::Call { function, args, .. } => {
                let mutable = self.by_name.get(function.as_str()).map(|callee| callee.mut_params.as_slice());
                mutable
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|at| args.get(*at).and_then(local))
                    .filter(|id| self.is_heap(id.0))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The locals that hold another value after an instruction, with that value.
    fn shares(&self, kind: &InstKind) -> Vec<(usize, usize, bool)> {
        let stores = |dest: &LocalId, items: Vec<&Operand>, inside: bool| -> Vec<(usize, usize, bool)> {
            items.into_iter().filter_map(local).map(|item| (dest.0, item.0, inside)).collect()
        };
        match kind {
            // An explicit `copy(...)` is a separate value on purpose.
            InstKind::Copy { dest, .. }
                if self.function.locals.get(dest.0).is_some_and(|local| local.copied) =>
            {
                Vec::new()
            }
            InstKind::Copy { dest, value } if self.is_heap(dest.0) && !self.places.contains_key(&dest.0) => {
                stores(dest, vec![value], false)
            }
            InstKind::Make { dest, .. }
            | InstKind::MakeCase { dest, .. }
            | InstKind::MakeList { dest, .. }
            | InstKind::MakeSet { dest, .. }
            | InstKind::MakeDict { dest, .. }
            | InstKind::MakeTuple { dest, .. }
            | InstKind::MakeClosure { dest, .. }
            | InstKind::MakeSome { dest, .. }
            | InstKind::MakeOk { dest, .. } => stores(dest, operands(kind), true),
            InstKind::Builtin { args, changes: true, .. } => {
                let Some(holder) = args.first().and_then(local) else { return Vec::new() };
                args.iter().skip(1).filter_map(local).map(|item| (holder.0, item.0, true)).collect()
            }
            InstKind::SetField { object, value, .. } | InstKind::SetIndex { object, value, .. } => {
                let holder = self.place(object.0).local;
                local(value)
                    .filter(|value| !self.places.contains_key(&value.0))
                    .map(|value| (holder, value.0, true))
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Records that `holder` holds `value`, and so whatever `value` holds too.
    fn share(&self, held: &mut Held, (holder, value): (usize, usize), inside: bool) {
        if !self.is_heap(value) {
            return;
        }
        let place = self.place(value);
        let inherited: Vec<(Place, bool)> = held
            .iter()
            .filter(|(by, ..)| *by == place.local)
            .map(|(_, value, deeper)| (value.clone(), inside || *deeper))
            .collect();
        for (value, inside) in std::iter::once((place, inside)).chain(inherited) {
            if value.local != holder {
                held.insert((holder, value, inside));
            }
        }
    }
}

/// Whether a value of this type may hold a shared buffer, directly or inside.
fn holds_heap(ty: &Type) -> bool {
    match ty {
        Type::Optional(inner) => holds_heap(inner),
        Type::Tuple(items) => items.iter().any(holds_heap),
        Type::Named(_) | Type::Generic(..) => true,
        other => on_the_heap(other),
    }
}

/// The places in a function where changing a value may copy it, with how many loops each is
/// in: `depths` has a count for each block.
pub(super) fn copies(
    function: &Function,
    by_name: &HashMap<&str, &Function>,
    depths: &[usize],
) -> Vec<(Span, Cost, usize)> {
    walk(function, by_name, depths, None)
}

/// Where a copy in a loop happens each time round: what's still shared when the loop comes
/// back to its start. What was shared before the loop is copied once, the first time.
pub(super) fn every_time(function: &Function, by_name: &HashMap<&str, &Function>) -> HashSet<(u32, u32)> {
    let depths = vec![0; function.blocks.len()];
    let mut found = HashSet::new();
    for (head, body) in super::loops::bodies(function) {
        let again = walk(function, by_name, &depths, Some((head, &body)));
        found.extend(again.into_iter().map(|(span, ..)| (span.start, span.end)));
    }
    found
}

/// Follows a function's blocks to where changing a value may copy it. With `fresh`, a loop's
/// start sees only what its own body shares, coming back round, and only copies in the loop
/// count.
fn walk(
    function: &Function,
    by_name: &HashMap<&str, &Function>,
    depths: &[usize],
    fresh: Option<(usize, &HashSet<usize>)>,
) -> Vec<(Span, Cost, usize)> {
    let flow = Flow {
        function,
        places: places_of_temporaries(function),
        live: live_after(function),
        callers: (0..function.params).filter(|at| !function.mut_params.contains(at)).collect(),
        by_name,
    };
    let count = function.blocks.len();
    let mut entry: Vec<Held> = vec![Held::new(); count];
    let mut found = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        found.clear();
        for (at, block) in function.blocks.iter().enumerate() {
            let mut held = entry.get(at).cloned().unwrap_or_default();
            let live = flow.live.get(at).map_or(&[][..], Vec::as_slice);
            let in_loop = fresh.is_none_or(|(_, body)| body.contains(&at));
            for (index, (inst, live)) in block.insts.iter().zip(live).enumerate() {
                if let Some(cost) = flow.step(&mut held, &inst.kind, (live, (at, index)))
                    && in_loop
                {
                    found.push((inst.span, cost, depths.get(at).copied().unwrap_or(0)));
                }
            }
            for target in block.end.targets() {
                // Coming into the loop from outside, nothing is shared yet.
                if fresh.is_some_and(|(head, body)| target.0 == head && !body.contains(&at)) {
                    continue;
                }
                if let Some(slot) = entry.get_mut(target.0) {
                    let before = slot.len();
                    slot.extend(held.iter().cloned());
                    changed |= slot.len() != before;
                }
            }
        }
    }
    found
}
