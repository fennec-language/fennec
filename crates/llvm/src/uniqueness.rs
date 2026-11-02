//! Which blocks under a local's fields are known to have no other owner, so a change through
//! them needn't check again. A check makes the block at a field path alone, and that holds
//! until something might give it another owner or put something else there: a read that owns
//! what it reads, a store, or a call that might do either with what it's given.
//!
//! Over the whole program, this works out which parameters each function leaves that way, so a
//! check before a call still holds after it, and which field paths of its `mut` parameters a
//! function changes through, so its callers can see to them once, where they may already know,
//! rather than it checking at every change.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_ir::{Function, InstKind, LocalId, Operand, Program, Terminator};

use crate::analysis::{Analysis, gives_back, visit};

/// A field path from a local: the block it leads to. A step can be a list's item, at the index
/// a local holds (`#3`) or at a number (`#=3`); `#*` stands for any item, in what's unsettled.
pub(crate) type FieldPath = (LocalId, Vec<String>);

/// Any item of a list, in a path.
const ANY_ITEM: &str = "#*";

/// The step to a list's item at this index, if it's one a path can name.
pub(crate) fn item_step(index: &Operand) -> Option<String> {
    match index {
        Operand::Local(local) => Some(format!("#{}", local.0)),
        Operand::Int(number) => Some(format!("#={number}")),
        _ => None,
    }
}

/// Whether a path, which may say any item, starts with a step for step match of another.
fn leads_to(under: &[String], path: &[String]) -> bool {
    path.len() >= under.len()
        && under
            .iter()
            .zip(path)
            .all(|(step, along)| step == along || (step == ANY_ITEM && along.starts_with('#')))
}

/// What an instruction may give another owner, or replace.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Unsettled {
    /// What's at the path, and everything under it.
    Under(LocalId, Vec<String>),
    /// Only what's under the path: what's at it was taken out and put back.
    Below(LocalId, Vec<String>),
    /// Anything through an item at the index this local held.
    Index(LocalId),
    /// Anything under any local.
    Everything,
}

impl Unsettled {
    /// Whether a block known alone at this field path might not be any more.
    pub(crate) fn hits(&self, (root, path): &FieldPath) -> bool {
        match self {
            Unsettled::Under(local, under) => local == root && leads_to(under, path),
            Unsettled::Below(local, under) => {
                local == root && path.len() > under.len() && leads_to(under, path)
            }
            Unsettled::Index(index) => {
                let step = format!("#{}", index.0);
                path.contains(&step)
            }
            Unsettled::Everything => true,
        }
    }

    /// What this says of a parameter, as said of what a call gives it.
    fn given(&self, known: &Analysis, arg: LocalId) -> Unsettled {
        let (Unsettled::Under(_, path) | Unsettled::Below(_, path)) = self else {
            return Unsettled::Everything;
        };
        let Some((root, mut base)) = reaches(known, arg, 8) else { return Unsettled::Everything };
        base.extend(path.iter().cloned());
        if base.len() > DEEPEST {
            base.truncate(DEEPEST);
            return Unsettled::Under(root, base);
        }
        match self {
            Unsettled::Below(..) => Unsettled::Below(root, base),
            _ => Unsettled::Under(root, base),
        }
    }
}

/// How deep a path a function's summary keeps: what's deeper counts as all of what's this
/// deep, so recursion through fields stops growing it.
const DEEPEST: usize = 4;

/// For each function, what it may give another owner, or replace, under each parameter.
pub(crate) type Summaries<'p> = HashMap<&'p str, Vec<Vec<Unsettled>>>;

/// Whether a local's value can be a block, or hold one.
fn counted(function: &Function, local: LocalId) -> bool {
    function
        .locals
        .get(local.0)
        .is_none_or(|held| !matches!(held.ty, Type::Unit | Type::None | Type::Bool | Type::Int | Type::Float))
}

fn counted_operand(function: &Function, operand: &Operand) -> bool {
    match operand {
        Operand::Local(local) => counted(function, *local),
        Operand::Str(_) | Operand::None => true,
        Operand::Int(_) | Operand::Float(_) | Operand::Bool(_) => false,
    }
}

/// Whether a local looks at what another holds, or holds it for a while, without an owner
/// of its own.
fn leans(known: &Analysis, local: LocalId) -> bool {
    known.is_borrowed(local)
        || known.is_taken(local)
        || known.alias(local).is_some()
        || known.item_home(local).is_some()
        || known.path_home(local).is_some()
}

/// Where what a local holds is: itself, if it owns it, or a field path from the local that
/// does. A path through a list's item goes on from the list's, which is as far as any check
/// is kept. `None` when that isn't known.
fn reaches(known: &Analysis, local: LocalId, depth: usize) -> Option<FieldPath> {
    if !leans(known, local) {
        return Some((local, Vec::new()));
    }
    let deeper = depth.checked_sub(1)?;
    if let Some((from, field)) = known.field_of(local) {
        let (root, mut path) = reaches(known, *from, deeper)?;
        path.push(field.clone());
        return Some((root, path));
    }
    if let Some((home, list)) = known.item_home(local) {
        let (root, mut path) = reaches(known, *home, deeper)?;
        path.extend(list.iter().cloned());
        path.push(ANY_ITEM.to_owned());
        return Some((root, path));
    }
    if let Some((home, list, _)) = known.path_home(local) {
        let (root, mut path) = reaches(known, *home, deeper)?;
        path.push(list.clone());
        path.push(ANY_ITEM.to_owned());
        return Some((root, path));
    }
    let whole = known.alias(local).or_else(|| known.lender(local))?;
    reaches(known, whole, deeper)
}

/// The field path from a local that owns it to the list a local holds, when it names one list.
pub(crate) fn list_place(known: &Analysis, local: LocalId) -> Option<FieldPath> {
    reaches(known, local, 8).filter(|(_, path)| !path.iter().any(|step| step == ANY_ITEM))
}

/// What a read that owns what it reads leaves with another owner.
fn owned(function: &Function, known: &Analysis, operand: &Operand, out: &mut Vec<Unsettled>) {
    if let Operand::Local(local) = operand
        && counted(function, *local)
    {
        out.push(
            reaches(known, *local, 8)
                .map_or(Unsettled::Everything, |(root, path)| Unsettled::Under(root, path)),
        );
    }
}

/// Which facts: that a block has no other owner, or that an index is inside a list, which only
/// changing the list's length, replacing it, or writing the index ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tracked {
    Owners,
    Lengths,
}

/// What an instruction may give another owner, or replace, given what each function it calls
/// may do so to under its parameters. For lengths, an owner more changes nothing.
pub(crate) fn unsettled(
    function: &Function,
    known: &Analysis,
    inst: &InstKind,
    (summaries, facts): (&Summaries<'_>, Tracked),
) -> Vec<Unsettled> {
    let mut out = Vec::new();
    let owned = |function: &Function, known: &Analysis, operand: &Operand, out: &mut Vec<Unsettled>| {
        if facts == Tracked::Owners {
            owned(function, known, operand, out);
        }
    };
    let put_back = |value: &Operand| matches!(value, Operand::Local(local) if known.is_taken(*local));
    match inst {
        InstKind::SetField { object, path, value } => {
            if counted_operand(function, value) {
                match reaches(known, *object, 8) {
                    Some((root, mut under)) => {
                        under.extend(path.iter().cloned());
                        out.push(if put_back(value) {
                            Unsettled::Below(root, under)
                        } else {
                            Unsettled::Under(root, under)
                        });
                    }
                    None => out.push(Unsettled::Everything),
                }
                if !put_back(value) {
                    owned(function, known, value, &mut out);
                }
            }
        }
        // An item put back where it was taken from is what was there. Another in its place
        // leaves the list as long as it was.
        InstKind::SetIndex { object, path, value, .. } => {
            let item_back = matches!(value, Operand::Local(local) if known.item_home(*local).is_some());
            if counted_operand(function, value) && !item_back && facts == Tracked::Owners {
                owned(function, known, value, &mut out);
                match reaches(known, *object, 8) {
                    Some((root, mut under)) => {
                        under.extend(path.iter().cloned());
                        under.push(ANY_ITEM.to_owned());
                        out.push(Unsettled::Under(root, under));
                    }
                    None => out.push(Unsettled::Everything),
                }
            }
        }
        InstKind::GetIndex { dest, object, .. } => {
            if let Operand::Local(object) = object
                && facts == Tracked::Owners
                && counted(function, *dest)
                && !leans(known, *dest)
            {
                match reaches(known, *object, 8) {
                    Some((root, mut path)) => {
                        path.push(ANY_ITEM.to_owned());
                        out.push(Unsettled::Under(root, path));
                    }
                    None => out.push(Unsettled::Everything),
                }
            }
        }
        InstKind::GetField { dest, object, field } => {
            if let Operand::Local(object) = object
                && facts == Tracked::Owners
                && counted(function, *dest)
                && !leans(known, *dest)
            {
                match reaches(known, *object, 8) {
                    Some((root, mut path)) => {
                        path.push(field.clone());
                        out.push(Unsettled::Under(root, path));
                    }
                    None => out.push(Unsettled::Everything),
                }
            }
        }
        InstKind::Format { .. }
        | InstKind::IsNone { .. }
        | InstKind::IsCase { .. }
        | InstKind::IsErr { .. }
        | InstKind::Binary { .. }
        | InstKind::Unary { .. }
        | InstKind::Constant { .. } => {}
        InstKind::Copy { dest, value } => {
            if !leans(known, *dest) {
                owned(function, known, value, &mut out);
            }
        }
        InstKind::Call { function: callee, args, .. } => {
            let theirs = summaries.get(callee.as_str());
            for (position, arg) in args.iter().enumerate() {
                match (theirs.and_then(|params| params.get(position)), arg) {
                    (Some(hits), Operand::Local(local)) => {
                        out.extend(hits.iter().map(|hit| hit.given(known, *local)));
                    }
                    (Some(_), _) => {}
                    (None, Operand::Local(local)) if facts == Tracked::Lengths => {
                        out.push(
                            reaches(known, *local, 8)
                                .map_or(Unsettled::Everything, |(root, path)| Unsettled::Below(root, path)),
                        );
                    }
                    (None, _) => owned(function, known, arg, &mut out),
                }
            }
        }
        InstKind::Builtin { name, args, changes, .. } => {
            let gives = gives_back(name);
            for (position, arg) in args.iter().enumerate() {
                if position > 0 || gives {
                    owned(function, known, arg, &mut out);
                }
            }
            // Changed in place, a list's items can move along or go, and a block inside can be
            // replaced by a bigger one.
            if *changes && let Some(Operand::Local(changed)) = args.first() {
                out.push(
                    reaches(known, *changed, 8)
                        .map_or(Unsettled::Everything, |(root, path)| Unsettled::Below(root, path)),
                );
            }
        }
        other => visit(other, |operand| owned(function, known, operand, &mut out), |_| {}),
    }
    visit(
        inst,
        |_| {},
        |dest| {
            out.push(Unsettled::Under(dest, Vec::new()));
            out.push(Unsettled::Index(dest));
        },
    );
    out
}

/// What a block's end may give another owner: what it hands back.
pub(crate) fn unsettled_by_end(
    function: &Function,
    known: &Analysis,
    end: &Terminator,
    facts: Tracked,
) -> Vec<Unsettled> {
    let mut out = Vec::new();
    if let Terminator::Return(Some(value)) = end
        && facts == Tracked::Owners
    {
        owned(function, known, value, &mut out);
    }
    out
}

/// What each function may give another owner, or replace, under each parameter. Worked out
/// over every function at once, from each doing nothing of the kind, as calls go both ways.
pub(crate) fn summarized<'p>(
    functions: &HashMap<&'p str, &'p Function>,
    analyses: &HashMap<&'p str, Analysis>,
    facts: Tracked,
) -> Summaries<'p> {
    let mut summaries: Summaries<'p> =
        functions.iter().map(|(name, function)| (*name, vec![Vec::new(); function.params])).collect();
    loop {
        let mut more = Vec::new();
        for (name, function) in functions {
            let Some(known) = analyses.get(name) else { continue };
            let mut hit = Vec::new();
            for block in &function.blocks {
                for inst in &block.insts {
                    hit.extend(unsettled(function, known, &inst.kind, (&summaries, facts)));
                }
                hit.extend(unsettled_by_end(function, known, &block.end, facts));
            }
            for unsettled in hit {
                let params: Vec<usize> = match &unsettled {
                    Unsettled::Under(local, _) | Unsettled::Below(local, _) if local.0 < function.params => {
                        vec![local.0]
                    }
                    Unsettled::Under(..) | Unsettled::Below(..) | Unsettled::Index(_) => Vec::new(),
                    Unsettled::Everything => (0..function.params).collect(),
                };
                for at in params {
                    let unsettled = match &unsettled {
                        Unsettled::Under(_, path) | Unsettled::Below(_, path) if path.len() > DEEPEST => {
                            Unsettled::Under(LocalId(at), path.iter().take(DEEPEST).cloned().collect())
                        }
                        other => other.clone(),
                    };
                    let known_already = summaries
                        .get(name)
                        .and_then(|params| params.get(at))
                        .is_some_and(|hits| hits.contains(&unsettled));
                    if !known_already && !more.contains(&(*name, at, unsettled.clone())) {
                        more.push((*name, at, unsettled));
                    }
                }
            }
        }
        if more.is_empty() {
            return summaries;
        }
        for (name, at, unsettled) in more {
            if let Some(hits) = summaries.get_mut(name).and_then(|params| params.get_mut(at)) {
                hits.push(unsettled);
            }
        }
    }
}

/// Every path from the root to a field path, the root's own included.
fn prefixes(path: &[String]) -> impl Iterator<Item = Vec<String>> + '_ {
    (0..=path.len()).map(|end| path.iter().take(end).cloned().collect())
}

/// The field paths of its `mut` parameters a function changes through, itself or in what it
/// calls, before anything there can say otherwise: those its callers see to, so it needn't.
/// None for a function called any other way than by name, or where a call hands it, beside
/// the parameter, something leaning on what's in it.
pub(crate) fn needed<'p>(
    program: &'p Program,
    functions: &HashMap<&'p str, &'p Function>,
    analyses: &HashMap<&'p str, Analysis>,
) -> HashMap<&'p str, Vec<FieldPath>> {
    let mut needs: HashMap<&str, HashSet<FieldPath>> = HashMap::new();
    for (name, function) in functions {
        let Some(known) = analyses.get(name) else { continue };
        let mut found = HashSet::new();
        let mutable = |local: LocalId| function.mut_params.contains(&local.0) && local.0 < function.params;
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            let mut paths: Vec<(LocalId, &[String])> = Vec::new();
            match &inst.kind {
                InstKind::SetIndex { object, path, .. } => paths.push((*object, path)),
                InstKind::SetField { object, path, .. } => {
                    paths.push((*object, path.split_last().map_or(&[][..], |(_, rest)| rest)));
                }
                InstKind::GetIndex { dest, .. } => {
                    if let Some((home, path)) = known.item_home(*dest) {
                        paths.push((*home, path));
                    }
                }
                _ => {}
            }
            for (root, path) in paths {
                if mutable(root) {
                    found.extend(prefixes(path).map(|prefix| (root, prefix)));
                }
            }
        }
        needs.insert(name, found);
    }
    // What a callee needs of a parameter, its caller needs of what it passes there.
    loop {
        let mut more = Vec::new();
        for (name, function) in functions {
            for inst in function.blocks.iter().flat_map(|block| &block.insts) {
                let InstKind::Call { function: callee, args, .. } = &inst.kind else { continue };
                let Some(theirs) = needs.get(callee.as_str()) else { continue };
                for (param, path) in theirs {
                    if let Some(Operand::Local(local)) = args.get(param.0)
                        && function.mut_params.contains(&local.0)
                        && local.0 < function.params
                        && needs.get(name).is_none_or(|mine| !mine.contains(&(*local, path.clone())))
                    {
                        more.push((*name, (*local, path.clone())));
                    }
                }
            }
        }
        if more.is_empty() {
            break;
        }
        for (name, path) in more {
            needs.entry(name).or_default().insert(path);
        }
    }
    // Only a call by name can see to what's needed.
    let mut other_ways: HashSet<&str> = HashSet::from(["main"]);
    for def in &program.generators {
        other_ways.insert(def.name.as_str());
        other_ways.insert(def.resume.as_str());
        other_ways.extend(def.abandon.as_deref());
    }
    for function in &program.constants {
        other_ways.insert(function.name.as_str());
    }
    for (name, function) in functions {
        let Some(known) = analyses.get(name) else { continue };
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            match &inst.kind {
                InstKind::MakeClosure { function: made, .. } => {
                    other_ways.insert(made.as_str());
                }
                InstKind::Call { function: callee, args, .. } => {
                    // What leans on a `mut` argument's insides could see it change unchecked.
                    let Some(target) = functions.get(callee.as_str()) else { continue };
                    for &param in &target.mut_params {
                        let Some(Operand::Local(given)) = args.get(param) else { continue };
                        let clash = args.iter().enumerate().any(|(position, arg)| {
                            position != param
                                && matches!(arg, Operand::Local(other)
                                    if counted(function, *other) && (other == given || leans(known, *other)))
                        });
                        if clash {
                            other_ways.insert(callee.as_str());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    needs
        .into_iter()
        .map(|(name, paths)| {
            let mut paths: Vec<FieldPath> =
                if other_ways.contains(name) { Vec::new() } else { paths.into_iter().collect() };
            paths.sort();
            (name, paths)
        })
        .collect()
}

/// The blocks a block's successors are.
fn after(function: &Function, at: usize) -> Vec<usize> {
    function
        .blocks
        .get(at)
        .map_or_else(Vec::new, |block| block.end.targets().iter().map(|next| next.0).collect())
}

/// The order to write blocks in so each comes after every block leading to it, but for the
/// ways back round loops: from the start, each after what it comes from. Blocks nothing leads
/// to go last.
pub(crate) fn written_order(function: &Function) -> Vec<usize> {
    let count = function.blocks.len();
    let mut seen = vec![false; count];
    let mut finished = Vec::with_capacity(count);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    if let Some(start) = seen.first_mut() {
        *start = true;
        stack.push((0, 0));
    }
    while let Some((block, next)) = stack.pop() {
        let targets = after(function, block);
        if let Some(target) = targets.get(next) {
            stack.push((block, next + 1));
            if let Some(slot) = seen.get_mut(*target)
                && !*slot
            {
                *slot = true;
                stack.push((*target, 0));
            }
        } else {
            finished.push(block);
        }
    }
    finished.reverse();
    finished.extend((0..count).filter(|block| !seen.get(*block).copied().unwrap_or(true)));
    finished
}

/// What's known alone coming into a block: what every way in leaves known, a way in being a
/// block written already, or a way back round a loop through here that nothing in the loop
/// unsettles, when the loop has no other way in.
pub(crate) fn coming_in(
    function: &Function,
    known: &Analysis,
    summaries: (&Summaries<'_>, Tracked),
    (at, before, done): (usize, &HashMap<usize, Vec<usize>>, &HashSet<usize>),
    (on_entry, left): (&[FieldPath], &HashMap<usize, Vec<FieldPath>>),
) -> Vec<FieldPath> {
    let ways_in = before.get(&at).map_or(&[][..], Vec::as_slice);
    let mut sides: Vec<&[FieldPath]> = Vec::new();
    if at == 0 {
        sides.push(on_entry);
    }
    let mut back = false;
    for from in ways_in {
        if done.contains(from) {
            sides.push(left.get(from).map_or(&[][..], Vec::as_slice));
        } else {
            back = true;
        }
    }
    let Some((first, rest)) = sides.split_first() else { return Vec::new() };
    let mut kept: Vec<FieldPath> =
        first.iter().filter(|path| rest.iter().all(|side| side.contains(path))).cloned().collect();
    if !back || kept.is_empty() {
        return kept;
    }
    // The loop: what's reached from here and gets back here.
    let mut reached = HashSet::from([at]);
    let mut todo = vec![at];
    while let Some(block) = todo.pop() {
        for next in after(function, block) {
            if reached.insert(next) {
                todo.push(next);
            }
        }
    }
    let mut returns = HashSet::from([at]);
    let mut todo = vec![at];
    while let Some(block) = todo.pop() {
        for from in before.get(&block).map_or(&[][..], Vec::as_slice) {
            if reached.contains(from) && returns.insert(*from) {
                todo.push(*from);
            }
        }
    }
    let one_way_in = ways_in.iter().all(|from| done.contains(from) || returns.contains(from))
        && returns.iter().filter(|block| **block != at).all(|block| {
            before.get(block).is_none_or(|froms| froms.iter().all(|from| returns.contains(from)))
        });
    if !one_way_in {
        return Vec::new();
    }
    for block in &returns {
        let Some(insts) = function.blocks.get(*block).map(|block| &block.insts) else { continue };
        for inst in insts {
            let hit = unsettled(function, known, &inst.kind, summaries);
            kept.retain(|path| !hit.iter().any(|unsettled| unsettled.hits(path)));
        }
    }
    kept
}
