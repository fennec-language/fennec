//! What's known about each function before emitting it: which of its checks can't fail, from
//! `fennec_ir::facts`, which lists keep one length, who a value can move to, and so which
//! functions can fail at all.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_ir::facts::{Facts, Range};
use fennec_ir::{
    Block, ChangedPlace, Function, GeneratorDef, Inst, InstKind, LocalId, Operand, PlaceStep, Program,
    Terminator,
};
use fennec_syntax::{BinOp, UnaryOp};

use crate::uniqueness::{ANY_ITEM, FieldPath, Summaries, Tracked, needed, summarized};

const FULL: Range = (i64::MIN, i64::MAX);
/// The most items anything in memory can have.
pub(crate) const MOST_ITEMS: i64 = 1 << 48;

/// What's known about one function.
/// Which parameters of a function it changes in place.
type Changes<'f> = &'f dyn Fn(&str) -> Vec<usize>;
/// Whether a function might give its parameter at a place another owner.
type Keeps<'f> = &'f dyn Fn(&str, usize) -> bool;

/// What a slice used as a key is of: the string, and its start and end as slicing takes them.
pub(crate) type SliceKey = (Operand, Option<Operand>, Option<Operand>);

pub(crate) struct Analysis {
    /// Locals an add, subtract, multiply or negate makes that can't overflow.
    safe: HashSet<usize>,
    /// Float locals that can't be zero.
    nonzero: HashSet<usize>,
    /// Locals holding a loop's count, with the limit they stay below and that they aren't negative.
    below: HashMap<usize, LocalId>,
    /// Locals holding the length of a list that keeps its length, and which list.
    length_of: HashMap<usize, usize>,
    /// How many instructions read and write each local.
    reads: Vec<u32>,
    writes: Vec<u32>,
    /// Locals whose value something else can come to hold: copied from, or passed to a call.
    shared: HashSet<usize>,
    /// Lists changed here, by an index or an append.
    changed: HashSet<usize>,
    /// Locals given to a call's `mut` parameter, which writes them with no write here to see.
    given_mut: HashSet<usize>,
    /// Locals whose one read can run again without their one write running first: in a loop
    /// that the write is outside of.
    read_again: HashSet<usize>,
    /// Locals read once, right away, from something that holds on to the value meanwhile, so
    /// they needn't own it.
    borrowed: HashSet<usize>,
    /// What each borrowing local borrows from, to let go once the borrow's done with.
    borrowed_from: HashMap<usize, LocalId>,
    /// Locals taking a field's value out to change it and put it back: it never has two owners.
    taken: HashSet<usize>,
    /// Locals taking an item out of a list to change it and put it back: the local or field
    /// the list is in, which has to be alone before the item's count says anything.
    items_taken: HashMap<usize, (LocalId, Vec<String>)>,
    /// Locals taking a field of a list's item out to change it, put back through the same place
    /// read again: `root.list[index].field`, as the root, the list's field and the index.
    path_taken: HashMap<usize, (LocalId, Vec<String>, Operand)>,
    /// `for mut` items taken out for the whole of the loop's body.
    loop_taken: HashSet<usize>,
    /// Locals holding a place a call changes, which is taken out for it: each place in order
    /// in the function's table.
    places_taken: HashMap<usize, usize>,
    /// The locals read on the way to such a place and back, which are never read at all, with
    /// the one holding the place. Each with the path to it from the root, for what's known.
    place_parts: HashMap<usize, LocalId>,
    place_paths: HashMap<usize, FieldPath>,
    /// Such locals taken out just before what changes them, as other arguments come first.
    places_waiting: HashSet<usize>,
    /// Locals written once, from a field of another: which, and the field.
    field_of: HashMap<usize, (LocalId, String)>,
    /// A slice of a string used only as a dict's key: the string and where it starts and ends.
    /// It's never made, as the runtime looks up and adds those bytes where they are.
    pub(crate) slice_keys: HashMap<usize, SliceKey>,
    /// The slice and its copy, which are never made.
    pub(crate) unmade: HashSet<usize>,
    /// Locals standing for a list that a `for` loop goes through while changing only the item
    /// it's on: a copy of the list would say the same, and cost a whole list.
    aliases: HashMap<usize, LocalId>,
    /// A block ending in a jump to a loop that fills a list made in that block: the list, and
    /// how many items the loop adds at most.
    pub(crate) reservations: HashMap<usize, (usize, Operand)>,
    /// Lists such a loop fills with nothing but zeros, one a turn from a count starting at 0:
    /// made whole, already zero, with the appends left out.
    pub(crate) zero_filled: HashSet<usize>,
    /// Copies, by block and place in it, whose source nothing reads again or leans on: the
    /// copy takes over its owner.
    last_copies: HashSet<(usize, usize)>,
    /// `enumerate(list)` results, and copies of them, never made: each with the list.
    pub(crate) enumerated: HashMap<usize, LocalId>,
    /// `map.items()` or `map.values()` where all that's done with what it gives is going through
    /// it in order: it's never made, as the loop walks the map's slots. Which map.
    pub(crate) map_walks: HashMap<usize, LocalId>,
    /// What such a loop reads each time round: which map, and whether it's a pair to take apart.
    pub(crate) walk_items: HashMap<usize, (LocalId, bool)>,
    /// Pairs read from those, never made: the list, and the index the pair is at.
    pub(crate) pairs: HashMap<usize, (LocalId, Operand)>,
}

/// What's known about every function, and which can fail.
pub(crate) struct Whole<'p> {
    pub(crate) functions: HashMap<&'p str, &'p Function>,
    pub(crate) analyses: HashMap<&'p str, Analysis>,
    pub(crate) can_fail: HashSet<&'p str>,
    /// Each generator, by its name and by the names of its functions.
    pub(crate) generators: HashMap<&'p str, &'p GeneratorDef>,
    /// What each function may give another owner, or replace, under each parameter.
    pub(crate) settles: Summaries<'p>,
    /// What each function may shorten, or replace, under each parameter.
    pub(crate) lengths: Summaries<'p>,
    /// For each function, the `list[str]` locals kept as the string they're joined into.
    pub(crate) builders: HashMap<&'p str, HashSet<usize>>,
    /// The field paths of `mut` parameters each function's callers make alone before calling.
    pub(crate) needs: HashMap<&'p str, Vec<FieldPath>>,
    /// The functions that give a constant's value, which keep it after the first time.
    pub(crate) constants: HashSet<&'p str>,
    /// The functions that can run on a thread `parallel` uses, which everything else can't.
    pub(crate) on_workers: HashSet<&'p str>,
}

/// What a function calls by name, a constant being a call of a function of no arguments, and a
/// generator the IR knows being a call of its resume function.
pub(crate) fn callees(function: &Function) -> impl Iterator<Item = &str> {
    function.blocks.iter().flat_map(|block| &block.insts).filter_map(|inst| match &inst.kind {
        InstKind::Call { function, .. } => Some(function.as_str()),
        InstKind::Constant { name, .. } | InstKind::Resume { known: Some(name), .. } => Some(name.as_str()),
        _ => None,
    })
}

fn range_of(facts: &Facts, operand: &Operand, block: usize) -> Range {
    match operand {
        Operand::Local(local) => facts.range_in(*local, block).unwrap_or(FULL),
        Operand::Int(value) => (*value, *value),
        _ => FULL,
    }
}

fn clamp(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}

/// The range of `a op b`, unbounded, if it says anything.
fn exact(op: BinOp, a: Range, b: Range) -> Option<(i128, i128)> {
    let (a0, a1, b0, b1) = (i128::from(a.0), i128::from(a.1), i128::from(b.0), i128::from(b.1));
    match op {
        BinOp::Add => Some((a0 + b0, a1 + b1)),
        BinOp::Sub => Some((a0 - b1, a1 - b0)),
        BinOp::Mul => {
            let corners = [a0 * b0, a0 * b1, a1 * b0, a1 * b1];
            Some((corners.into_iter().min()?, corners.into_iter().max()?))
        }
        _ => None,
    }
}

/// What an instruction reads and writes, by local, to count them.
pub(crate) fn visit(inst: &InstKind, mut read: impl FnMut(&Operand), mut write: impl FnMut(LocalId)) {
    let mut local = |id: LocalId| read(&Operand::Local(id));
    match inst {
        InstKind::SetField { object, value, .. } => {
            local(*object);
            read(value);
        }
        InstKind::Slice { dest, object, start, end } => {
            read(object);
            start.iter().for_each(&mut read);
            end.iter().for_each(&mut read);
            write(*dest);
        }
        InstKind::SetIndex { object, index, value, .. } => {
            local(*object);
            read(index);
            read(value);
        }
        InstKind::Copy { dest, value }
        | InstKind::Unary { dest, value, .. }
        | InstKind::GetField { dest, object: value, .. }
        | InstKind::GetTupleItem { dest, value, .. }
        | InstKind::MakeSome { dest, value }
        | InstKind::GetSome { dest, value }
        | InstKind::IsNone { dest, value }
        | InstKind::IsCase { dest, value, .. }
        | InstKind::GetCaseField { dest, value, .. }
        | InstKind::MakeErr { dest, error: value }
        | InstKind::IsErr { dest, value }
        | InstKind::GetOk { dest, value }
        | InstKind::GetErr { dest, value } => {
            read(value);
            write(*dest);
        }
        InstKind::MakeOk { dest, value } => {
            value.iter().for_each(&mut read);
            write(*dest);
        }
        InstKind::Binary { dest, lhs, rhs, .. } => {
            read(lhs);
            read(rhs);
            write(*dest);
        }
        InstKind::Make { dest, fields: items, .. }
        | InstKind::MakeCase { dest, fields: items, .. }
        | InstKind::MakeList { dest, items }
        | InstKind::MakeTuple { dest, items }
        | InstKind::MakeClosure { dest, captures: items, .. }
        | InstKind::MakeSet { dest, items } => {
            items.iter().for_each(&mut read);
            write(*dest);
        }
        InstKind::GetIndex { dest, object, index } => {
            read(object);
            read(index);
            write(*dest);
        }
        InstKind::Format { dest, parts } => {
            for part in parts {
                if let fennec_ir::FormatPart::Value { value, .. } = part {
                    read(value);
                }
            }
            write(*dest);
        }
        InstKind::MakeDict { dest, pairs } => {
            for (key, value) in pairs {
                read(key);
                read(value);
            }
            write(*dest);
        }
        InstKind::CallClosure { dest, closure, args } => {
            read(closure);
            args.iter().for_each(&mut read);
            if let Some(dest) = dest {
                write(*dest);
            }
        }
        InstKind::Constant { dest, .. } => write(*dest),
        InstKind::MakeGenerator { dest, args, .. } => {
            for (_, value) in args {
                read(value);
            }
            write(*dest);
        }
        InstKind::GetSlot { dest, generator, .. } | InstKind::Resume { dest, generator, .. } => {
            read(generator);
            write(*dest);
        }
        InstKind::SetSlot { generator, value, .. } => {
            read(generator);
            read(value);
        }
        InstKind::Builtin { dest, args, .. } | InstKind::Call { dest, args, .. } => {
            args.iter().for_each(&mut read);
            if let Some(dest) = dest {
                write(*dest);
            }
        }
        // What this backend doesn't take yet stops it when it's emitted.
        _ => {}
    }
}

/// What the facts leave to say about one instruction.
fn note(
    function: &Function,
    facts: &mut Facts,
    known: &mut Analysis,
    stepped: &HashSet<usize>,
    (block, inst): (usize, &InstKind),
) {
    match inst {
        InstKind::Copy { dest, value } => {
            facts.note_write(*dest, range_of(facts, value, block));
            if let Operand::Local(from) = value {
                if known.nonzero.contains(&from.0) {
                    known.nonzero.insert(dest.0);
                }
                if facts.holds_one_value(*dest) {
                    if let Some(list) = known.length_of.get(&from.0).copied() {
                        known.length_of.insert(dest.0, list);
                    }
                    if let Some(count) =
                        function.counts.iter().find(|count| count.count == *from && !count.down)
                    {
                        known.below.insert(dest.0, count.stop);
                    }
                }
            }
        }
        InstKind::Binary { dest, op, lhs, rhs } if function.type_of(lhs) == Some(Type::Int) => {
            if stepped.contains(&dest.0) && *lhs == Operand::Local(*dest) {
                known.safe.insert(dest.0);
                return;
            }
            let (a, b) = (range_of(facts, lhs, block), range_of(facts, rhs, block));
            if let Some((low, high)) = exact(*op, a, b) {
                if low >= i128::from(i64::MIN) && high <= i128::from(i64::MAX) {
                    known.safe.insert(dest.0);
                }
                facts.note_write(*dest, (clamp(low), clamp(high)));
            } else if let Operand::Int(by @ 1..) = rhs {
                let range = match op {
                    BinOp::FloorDiv => Some((a.0.div_euclid(*by), a.1.div_euclid(*by))),
                    BinOp::Mod => Some((0, by - 1)),
                    _ => None,
                };
                if let Some(range) = range {
                    facts.note_write(*dest, range);
                }
            }
        }
        InstKind::Unary { dest, op: UnaryOp::Neg, value } => {
            let range = range_of(facts, value, block);
            if function.type_of(value) == Some(Type::Int) && range.0 > i64::MIN {
                known.safe.insert(dest.0);
                facts.note_write(*dest, (-range.1, -range.0));
            }
        }
        InstKind::Builtin { dest: Some(dest), name, args, .. } => match name.as_str() {
            "float" => {
                let (low, high) = args.first().map_or(FULL, |arg| range_of(facts, arg, block));
                if low > 0 || high < 0 {
                    known.nonzero.insert(dest.0);
                }
            }
            "list.len" => {
                facts.note_write(*dest, (0, MOST_ITEMS));
                if let (Some(Operand::Local(list)), true) = (args.first(), facts.holds_one_value(*dest)) {
                    known.length_of.insert(dest.0, list.0);
                }
            }
            _ => {}
        },
        _ => {}
    }
}

/// Counts what reads and writes each local, and notes which lists change or are handed on.
/// Gives back those that can change length.
fn count_uses(
    function: &Function,
    (changes, keeps): (Changes<'_>, Keeps<'_>),
    known: &mut Analysis,
) -> HashSet<usize> {
    let mut grown = HashSet::new();
    for block in &function.blocks {
        for inst in &block.insts {
            let (mut reads, mut writes) = (Vec::new(), Vec::new());
            visit(&inst.kind, |operand| reads.push(operand.clone()), |local| writes.push(local));
            for operand in &reads {
                if let Operand::Local(local) = operand
                    && let Some(held) = known.reads.get_mut(local.0)
                {
                    *held += 1;
                }
            }
            for local in writes {
                if let Some(held) = known.writes.get_mut(local.0) {
                    *held += 1;
                }
            }
            match &inst.kind {
                InstKind::Builtin { name, args, changes, .. } if *changes || name == "list.append" => {
                    if let Some(Operand::Local(list)) = args.first() {
                        grown.insert(list.0);
                        known.changed.insert(list.0);
                    }
                }
                InstKind::SetIndex { object, .. } => {
                    known.changed.insert(object.0);
                }
                InstKind::Call { function: callee, args, .. } => {
                    for at in changes(callee) {
                        if let Some(Operand::Local(list)) = args.get(at) {
                            grown.insert(list.0);
                            known.given_mut.insert(list.0);
                        }
                    }
                    // Only what the callee might keep comes to have another owner.
                    for (at, arg) in args.iter().enumerate() {
                        if let Operand::Local(local) = arg
                            && keeps(callee, at)
                        {
                            known.shared.insert(local.0);
                        }
                    }
                }
                _ => {}
            }
        }
        if let Terminator::Return(Some(Operand::Local(local)))
        | Terminator::Branch { cond: Operand::Local(local), .. } = &block.end
            && let Some(held) = known.reads.get_mut(local.0)
        {
            *held += 1;
        }
    }
    grown
}

fn analyze(
    function: &Function,
    changes: &dyn Fn(&str) -> Vec<usize>,
    keeps: &dyn Fn(&str, usize) -> bool,
) -> Analysis {
    let count = function.locals.len();
    let mut known = Analysis {
        safe: HashSet::new(),
        nonzero: HashSet::new(),
        below: HashMap::new(),
        length_of: HashMap::new(),
        reads: vec![0; count],
        writes: vec![0; count],
        shared: HashSet::new(),
        changed: HashSet::new(),
        given_mut: HashSet::new(),
        read_again: HashSet::new(),
        borrowed: HashSet::new(),
        borrowed_from: HashMap::new(),
        taken: HashSet::new(),
        items_taken: HashMap::new(),
        path_taken: HashMap::new(),
        loop_taken: HashSet::new(),
        places_taken: HashMap::new(),
        place_parts: HashMap::new(),
        place_paths: HashMap::new(),
        places_waiting: HashSet::new(),
        field_of: HashMap::new(),
        slice_keys: HashMap::new(),
        unmade: HashSet::new(),
        aliases: HashMap::new(),
        reservations: HashMap::new(),
        zero_filled: HashSet::new(),
        last_copies: HashSet::new(),
        enumerated: HashMap::new(),
        map_walks: HashMap::new(),
        walk_items: HashMap::new(),
        pairs: HashMap::new(),
    };
    let grown = count_uses(function, (changes, keeps), &mut known);
    find_reads_again(function, &mut known);
    find_place_takes(function, changes, &mut known);
    find_borrows(function, changes, &mut known);
    find_live_borrows(function, changes, &mut known);
    find_path_takes(function, &mut known);
    find_aliases(function, &mut known);
    settle_place_takes(function, &mut known);
    find_loop_takes(function, &mut known);
    find_last_copies(function, &mut known);
    // A copy shares its source, unless the source is never read again, and a block put inside
    // something else is shared by that.
    let placed =
        function.blocks.iter().enumerate().flat_map(|(at, block)| {
            block.insts.iter().enumerate().map(move |(index, inst)| ((at, index), inst))
        });
    for (place, inst) in placed {
        match &inst.kind {
            // A copy standing for the list a loop goes through owns nothing.
            InstKind::Copy { dest, .. } if known.aliases.contains_key(&dest.0) => {}
            InstKind::Copy { .. } if known.last_copies.contains(&place) => {}
            InstKind::Copy { value: Operand::Local(from), .. }
            | InstKind::MakeSome { value: Operand::Local(from), .. }
            | InstKind::GetSome { value: Operand::Local(from), .. }
                if !known.moves(*from) =>
            {
                known.shared.insert(from.0);
            }
            InstKind::Make { fields: items, .. }
            | InstKind::MakeCase { fields: items, .. }
            | InstKind::MakeList { items, .. } => {
                for item in items {
                    if let Operand::Local(local) = item {
                        known.shared.insert(local.0);
                    }
                }
            }
            InstKind::SetField { value: Operand::Local(local), .. }
            | InstKind::SetIndex { value: Operand::Local(local), .. } => {
                known.shared.insert(local.0);
            }
            InstKind::Builtin { name, args, .. } if name == "list.append" => {
                if let Some(Operand::Local(local)) = args.get(1) {
                    known.shared.insert(local.0);
                }
            }
            _ => {}
        }
    }
    let mut facts = Facts::of(function, changes);
    let stepped: HashSet<usize> = function.counts.iter().map(|count| count.count.0).collect();
    for (block, written) in function.blocks.iter().enumerate() {
        for inst in &written.insts {
            note(function, &mut facts, &mut known, &stepped, (block, &inst.kind));
        }
    }
    // A length only says something about a list that keeps it.
    let writes = known.writes.clone();
    let params = function.params;
    known.length_of.retain(|_, list| {
        let written = writes.get(*list).copied().unwrap_or(2);
        !grown.contains(list) && written == u32::from(*list >= params)
    });
    known.below.retain(|local, _| facts.range_in(LocalId(*local), 0).is_some_and(|range| range.0 >= 0));
    find_reservations(function, &stepped, &mut known);
    find_slice_keys(function, &mut known);
    find_unmade_enumerates(function, &mut known);
    known
}

/// `d.get(text[a:b])` and `d[text[a:b]] = v`, with the slice used for nothing else and what it
/// was made from unchanged after: the runtime finds and adds those bytes where they are.
fn find_slice_keys(function: &Function, known: &mut Analysis) {
    let insts: Vec<&InstKind> =
        function.blocks.iter().flat_map(|block| &block.insts).map(|inst| &inst.kind).collect();
    let steady = |operand: &Operand, known: &Analysis| match operand {
        Operand::Local(local) => known.writes.get(local.0).copied().unwrap_or(2) <= 1,
        Operand::Int(_) => true,
        _ => false,
    };
    let str_keyed = |dict: &Operand| match dict {
        Operand::Local(local) => matches!(
            function.locals.get(local.0).map(|local| &local.ty),
            Some(Type::Dict(key, _)) if **key == Type::Str
        ),
        _ => false,
    };
    for inst in &insts {
        let InstKind::Slice { dest: slice, object, start, end } = inst else { continue };
        let Operand::Local(text) = object else { continue };
        if function.locals.get(text.0).map(|local| &local.ty) != Some(&Type::Str)
            || known.writes.get(slice.0) != Some(&1)
            || !steady(object, known)
            || !start.iter().chain(end).all(|at| steady(at, known))
        {
            continue;
        }
        // Copied once to the local the IR uses as the key, or used as it is.
        let copied = insts.iter().find_map(|inst| match inst {
            InstKind::Copy { dest, value: Operand::Local(from) } if from == slice => Some(*dest),
            _ => None,
        });
        let key = match copied {
            Some(key) if known.reads.get(slice.0) == Some(&1) && known.writes.get(key.0) == Some(&1) => key,
            Some(_) => continue,
            None => *slice,
        };
        let as_key = Operand::Local(key);
        let mut uses = 0;
        for inst in &insts {
            match inst {
                InstKind::Builtin { name, args, .. }
                    if name == "dict.get"
                        && args.len() == 2
                        && args.get(1) == Some(&as_key)
                        && args.first().is_some_and(str_keyed) =>
                {
                    uses += 1;
                }
                InstKind::SetIndex { object, path, index, value }
                    if *index == as_key
                        && path.is_empty()
                        && *value != as_key
                        && str_keyed(&Operand::Local(*object)) =>
                {
                    uses += 1;
                }
                _ => {}
            }
        }
        if uses == 0 || known.reads.get(key.0).copied() != Some(uses) {
            continue;
        }
        known.slice_keys.insert(key.0, (object.clone(), start.clone(), end.clone()));
        known.unmade.insert(slice.0);
        known.unmade.insert(key.0);
    }
}

/// Whether a block can get back to itself without going through another.
fn comes_back(function: &Function, block: usize, without: usize) -> bool {
    let mut seen = HashSet::new();
    let mut waiting: Vec<usize> = function
        .blocks
        .get(block)
        .map(|held| held.end.targets().iter().map(|target| target.0).collect())
        .unwrap_or_default();
    while let Some(at) = waiting.pop() {
        if at == block {
            return true;
        }
        if at != without
            && seen.insert(at)
            && let Some(held) = function.blocks.get(at)
        {
            waiting.extend(held.end.targets().iter().map(|target| target.0));
        }
    }
    false
}

/// Finds the locals written and read once, whose read can happen more times than their write.
fn find_reads_again(function: &Function, known: &mut Analysis) {
    let mut written_in: HashMap<usize, usize> = HashMap::new();
    let mut read_in: HashMap<usize, usize> = HashMap::new();
    for (at, block) in function.blocks.iter().enumerate() {
        for inst in &block.insts {
            visit(
                &inst.kind,
                |operand| {
                    if let Operand::Local(local) = operand {
                        read_in.insert(local.0, at);
                    }
                },
                |local| {
                    written_in.insert(local.0, at);
                },
            );
        }
        if let Terminator::Return(Some(Operand::Local(local)))
        | Terminator::Branch { cond: Operand::Local(local), .. } = &block.end
        {
            read_in.insert(local.0, at);
        }
    }
    for (local, read) in read_in {
        let written = written_in.get(&local).copied().unwrap_or(usize::MAX);
        if read != written && comes_back(function, read, written) {
            known.read_again.insert(local);
        }
    }
}

/// Whether nothing an instruction does can change or let go of a value something else holds.
fn is_quiet(inst: &InstKind) -> bool {
    matches!(
        inst,
        InstKind::Binary { .. }
            | InstKind::Unary { .. }
            | InstKind::GetField { .. }
            | InstKind::GetIndex { .. }
            | InstKind::GetTupleItem { .. }
            | InstKind::GetCaseField { .. }
            | InstKind::IsCase { .. }
            | InstKind::IsNone { .. }
    )
}

/// Whether an instruction could change one of these locals: write it, change what's inside it,
/// or give it to a call as `mut`.
fn may_change(inst: &InstKind, lenders: &[LocalId], changes: &dyn Fn(&str) -> Vec<usize>) -> bool {
    let held = |operand: &Operand| matches!(operand, Operand::Local(local) if lenders.contains(local));
    let mut written = false;
    visit(inst, |_| {}, |local| written |= lenders.contains(&local));
    written
        || match inst {
            InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } => lenders.contains(object),
            InstKind::Builtin { args, changes: true, .. } => args.first().is_some_and(held),
            InstKind::Call { .. } => !leaves_alone(inst, lenders, changes),
            InstKind::Lock { .. } | InstKind::Unlock { .. } => true,
            _ => false,
        }
}

/// Whether an instruction reads `local` without needing an owner of its own for it: looking
/// inside it, comparing it, or lending it to a call that doesn't take it `mut`.
fn reads_without_owning(inst: &InstKind, local: LocalId, changes: &dyn Fn(&str) -> Vec<usize>) -> bool {
    let held = Operand::Local(local);
    if only_looks(inst, local) {
        return true;
    }
    match inst {
        InstKind::Binary { lhs, rhs, .. } => *lhs == held || *rhs == held,
        InstKind::Unary { value, .. } | InstKind::GetTupleItem { value, .. } => *value == held,
        InstKind::Call { function, args, .. } => {
            let changed = changes(function);
            args.iter().enumerate().all(|(at, arg)| *arg != held || !changed.contains(&at))
        }
        _ => false,
    }
}

/// A read that may borrow: where it's made, what it's read from, and everything that has to
/// stay as it is while it's live.
struct Candidate {
    defined: (usize, usize),
    source: LocalId,
    lenders: Vec<LocalId>,
}

/// Finds more reads of a field or item that need no owner of their own: read any number of
/// times, in any block, but only looked at, while nothing could change what they came from.
/// What they came from has to be a parameter, or borrowed from one, so it outlives them.
fn find_live_borrows(function: &Function, changes: &dyn Fn(&str) -> Vec<usize>, known: &mut Analysis) {
    let is_param = |local: LocalId| local.0 < function.params;
    let mut candidates: HashMap<usize, Candidate> = HashMap::new();
    let mut copies: HashSet<usize> = HashSet::new();
    for (at, block) in function.blocks.iter().enumerate() {
        for (index, inst) in block.insts.iter().enumerate() {
            // A candidate copied once to a local of its own, which borrows from it in turn.
            if let InstKind::Copy { dest, value: Operand::Local(from) } = &inst.kind
                && known.writes.get(dest.0) == Some(&1)
                && dest.0 >= function.params
                && let Some(copied) = candidates.get(&from.0)
            {
                let lenders = std::iter::once(*from).chain(copied.lenders.iter().copied()).collect();
                candidates.insert(dest.0, Candidate { defined: (at, index), source: *from, lenders });
                copies.insert(dest.0);
                continue;
            }
            let (dest, source) = match &inst.kind {
                InstKind::GetField { dest, object: Operand::Local(from), .. }
                | InstKind::GetIndex { dest, object: Operand::Local(from), .. }
                | InstKind::GetCaseField { dest, value: Operand::Local(from), .. } => (*dest, *from),
                _ => continue,
            };
            let source_ty = function.locals.get(source.0).map(|local| &local.ty);
            let id_map = matches!(source_ty, Some(Type::IdMap(_)));
            // A string's character is a string of its own, made by the indexing, not one held
            // inside what it came from, so there's nothing to borrow.
            let text = matches!(source_ty, Some(Type::Str));
            if known.writes.get(dest.0) != Some(&1)
                || dest == source
                || known.borrowed.contains(&dest.0)
                || known.taken.contains(&dest.0)
                || dest.0 < function.params
                || ((id_map || text) && matches!(&inst.kind, InstKind::GetIndex { .. }))
            {
                continue;
            }
            let mut lenders = vec![source];
            let mut lasting = true;
            while let Some(&last) = lenders.last() {
                if is_param(last) {
                    break;
                }
                match known.borrowed_from.get(&last.0) {
                    Some(lender) if known.borrowed.contains(&last.0) => lenders.push(*lender),
                    _ => {
                        lasting = false;
                        break;
                    }
                }
            }
            // Anything that is one of them changes them too.
            let aliased: Vec<LocalId> = (0..function.locals.len())
                .map(LocalId)
                .filter(|local| known.alias(*local).is_some_and(|of| lenders.contains(&of)))
                .collect();
            lenders.extend(aliased);
            if lasting {
                candidates.insert(dest.0, Candidate { defined: (at, index), source, lenders });
            }
        }
    }
    if candidates.is_empty() {
        return;
    }
    let live_out = live_after_blocks(function);
    for (at, block) in function.blocks.iter().enumerate() {
        let mut live = live_out.get(at).cloned().unwrap_or_default();
        match &block.end {
            // Given back, it needs an owner.
            Terminator::Return(Some(Operand::Local(local))) => {
                candidates.remove(&local.0);
                live.insert(local.0);
            }
            Terminator::Branch { cond: Operand::Local(local), .. }
            | Terminator::NoMatch { value: Operand::Local(local), .. } => {
                live.insert(local.0);
            }
            _ => {}
        }
        for (index, inst) in block.insts.iter().enumerate().rev() {
            let (mut reads, mut writes) = (Vec::new(), Vec::new());
            visit(&inst.kind, |operand| reads.extend(local_of(operand)), |local| writes.push(local.0));
            candidates.retain(|dest, Candidate { defined, lenders, .. }| {
                // Copied to a local borrowing it in turn, like the list a loop goes over.
                let copied = matches!(&inst.kind, InstKind::Copy { dest: to, value: Operand::Local(from) }
                    if from.0 == *dest && copies.contains(&to.0));
                if reads.contains(dest)
                    && !copied
                    && !reads_without_owning(&inst.kind, LocalId(*dest), changes)
                {
                    return false;
                }
                !(live.contains(dest) && *defined != (at, index) && may_change(&inst.kind, lenders, changes))
            });
            for write in writes {
                live.remove(&write);
            }
            live.extend(reads);
        }
    }
    // A copy borrows from what it copied, so that has to be borrowed too.
    loop {
        let gone: Vec<usize> = candidates
            .iter()
            .filter(|(dest, held)| copies.contains(dest) && !candidates.contains_key(&held.source.0))
            .map(|(dest, _)| *dest)
            .collect();
        if gone.is_empty() {
            break;
        }
        for dest in gone {
            candidates.remove(&dest);
        }
    }
    for (dest, Candidate { source, .. }) in candidates {
        known.borrowed.insert(dest);
        known.borrowed_from.insert(dest, source);
    }
}

/// Whether a call leaves these locals as they are: only what's given `mut` can change, and
/// nothing else holds a local's value.
fn leaves_alone(inst: &InstKind, lenders: &[LocalId], changes: &dyn Fn(&str) -> Vec<usize>) -> bool {
    let InstKind::Call { dest, function, args } = inst else { return false };
    if dest.is_some_and(|dest| lenders.contains(&dest)) {
        return false;
    }
    changes(function)
        .iter()
        .all(|at| !matches!(args.get(*at), Some(Operand::Local(held)) if lenders.contains(held)))
}

/// Whether an instruction only looks inside `local`, taking no owner of its own.
fn only_looks(inst: &InstKind, local: LocalId) -> bool {
    let held = Operand::Local(local);
    match inst {
        InstKind::GetIndex { object, index, .. } => *object == held && *index != held,
        InstKind::GetField { object, .. } => *object == held,
        InstKind::IsNone { value, .. }
        | InstKind::IsCase { value, .. }
        | InstKind::GetCaseField { value, .. } => *value == held,
        InstKind::Builtin { name, args, .. } => {
            args.first() == Some(&held)
                && !args.iter().skip(1).any(|arg| *arg == held)
                && matches!(
                    name.as_str(),
                    "list.len"
                        | "dict.get"
                        | "dict.len"
                        | "dict.contains"
                        | "set.len"
                        | "set.contains"
                        | "str.len"
                )
        }
        _ => false,
    }
}

/// Finds reads of a field or item whose value needs no owner of its own: read once, by something
/// that only looks inside it, before anything could change what it came from; and a field
/// taken out, changed and put back, with nothing able to see it in between.
fn find_borrows(function: &Function, changes: &dyn Fn(&str) -> Vec<usize>, known: &mut Analysis) {
    find_text_growing_in_a_field(function, known);
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        // A character's block lasts the whole run.
        if let InstKind::Builtin { dest: Some(dest), name, .. } = &inst.kind
            && name == "str.char_at_byte"
        {
            known.borrowed.insert(dest.0);
        }
        if let InstKind::GetField { dest, object: Operand::Local(from), field } = &inst.kind
            && known.writes.get(dest.0) == Some(&1)
        {
            known.field_of.insert(dest.0, (*from, field.clone()));
        }
    }
    for block in &function.blocks {
        // Where a borrowed list came from, for an item taken out of it.
        let mut from_field: HashMap<usize, (LocalId, Vec<String>)> = HashMap::new();
        for (at, inst) in block.insts.iter().enumerate() {
            let (dest, source) = match &inst.kind {
                InstKind::GetField { dest, object: Operand::Local(from), .. }
                | InstKind::GetIndex { dest, object: Operand::Local(from), .. }
                | InstKind::GetCaseField { dest, value: Operand::Local(from), .. } => (*dest, *from),
                _ => continue,
            };
            if known.writes.get(dest.0) != Some(&1) || dest == source {
                continue;
            }
            let later = block.insts.get(at + 1..).unwrap_or_default();
            if known.reads.get(dest.0) == Some(&1) {
                // What it came from, and what that borrows from, which nothing may change first.
                let mut lenders = vec![source];
                while let Some(lender) = lenders.last().and_then(|held| known.borrowed_from.get(&held.0)) {
                    lenders.push(*lender);
                }
                for next in later {
                    if only_looks(&next.kind, dest) {
                        known.borrowed.insert(dest.0);
                        known.borrowed_from.insert(dest.0, source);
                        break;
                    }
                    if !is_quiet(&next.kind) && !leaves_alone(&next.kind, &lenders, changes) {
                        break;
                    }
                }
            }
            // An `IdMap`'s items aren't a list's, so they're read and written as they are.
            let id_map = |local: LocalId| {
                matches!(function.locals.get(local.0).map(|local| &local.ty), Some(Type::IdMap(_)))
            };
            if matches!(&inst.kind, InstKind::GetIndex { .. }) && id_map(source) {
                continue;
            }
            if let InstKind::GetIndex { index, .. } = &inst.kind {
                let home = from_field.get(&source.0).cloned().unwrap_or((source, Vec::new()));
                for next in later {
                    match &next.kind {
                        InstKind::SetIndex { object, path, index: put_at, value: Operand::Local(put) }
                            if *object == home.0 && *path == home.1 && put_at == index && *put == dest =>
                        {
                            known.taken.insert(dest.0);
                            known.items_taken.insert(dest.0, home);
                            break;
                        }
                        InstKind::SetField { object, value, .. }
                            if *object == dest && !matches!(value, Operand::Local(held) if *held == dest) => {
                        }
                        // Something inside it, changed directly, or taken out and changed on the way back.
                        InstKind::SetIndex { object, .. } if *object == dest => {}
                        InstKind::SetIndex { object, path, .. } if *object != home.0 && path.is_empty() => {}
                        other if is_quiet(other) => {}
                        _ => break,
                    }
                }
                continue;
            }
            let InstKind::GetField { field, .. } = &inst.kind else { continue };
            if known.borrowed.contains(&dest.0) {
                from_field.insert(dest.0, (source, vec![field.clone()]));
            }
            for next in later {
                match &next.kind {
                    InstKind::SetField { object, path, value: Operand::Local(put) }
                        if *object == source
                            && *put == dest
                            && path.as_slice() == std::slice::from_ref(field) =>
                    {
                        known.taken.insert(dest.0);
                        break;
                    }
                    InstKind::Builtin { name, args, .. }
                        if name == "list.append" && args.first() == Some(&Operand::Local(dest)) => {}
                    InstKind::Builtin { name, args, changes: true, .. }
                        if name.starts_with("IdMap.")
                            && args.first() == Some(&Operand::Local(dest))
                            && !args.iter().skip(1).any(|arg| {
                                *arg == Operand::Local(dest) || *arg == Operand::Local(source)
                            }) => {}
                    // A method written in Fennec changing it, like `self.inner.add(x)`, given
                    // it alone and nothing else of what it came from.
                    InstKind::Call { function: callee, args, .. }
                        if args.iter().filter(|arg| **arg == Operand::Local(dest)).count() == 1
                            && changes(callee)
                                .iter()
                                .any(|at| args.get(*at) == Some(&Operand::Local(dest)))
                            && !args.contains(&Operand::Local(source)) => {}
                    InstKind::SetIndex { object, path, .. } if *object == dest && path.is_empty() => {}
                    // Making something else, from neither of them.
                    other @ (InstKind::Make { .. }
                    | InstKind::MakeList { .. }
                    | InstKind::MakeTuple { .. }
                    | InstKind::MakeCase { .. }
                    | InstKind::Copy { .. })
                        if !reads_local(other, dest) && !reads_local(other, source) => {}
                    other if is_quiet(other) => {}
                    _ => break,
                }
            }
        }
    }
}

/// `object.field += text`, which the IR writes as a local read from the field, added to and put
/// straight back: the text is taken out, not given another owner, so it grows where it is.
fn find_text_growing_in_a_field(function: &Function, known: &mut Analysis) {
    for block in &function.blocks {
        for insts in block.insts.windows(3) {
            let [read, grow, put] = insts else { continue };
            let InstKind::GetField { dest, object: Operand::Local(from), field } = &read.kind else {
                continue;
            };
            let InstKind::Binary { dest: grown, op: BinOp::Add, lhs, .. } = &grow.kind else { continue };
            let InstKind::SetField { object, path, value: Operand::Local(put_back) } = &put.kind else {
                continue;
            };
            let is_text = matches!(function.locals.get(dest.0).map(|local| &local.ty), Some(Type::Str));
            let same_local = (grown, lhs, put_back) == (dest, &Operand::Local(*dest), dest);
            let same_place = (object, path.as_slice()) == (from, std::slice::from_ref(field));
            if is_text && same_local && same_place {
                known.taken.insert(dest.0);
            }
        }
    }
}

/// The places calls change that can be taken out, from what the lowering says of them: nothing
/// reads the place between its being read and being written back but the call changing it.
/// Anything else keeps its own owner, and the call's change makes a copy, which is right.
fn find_place_takes(function: &Function, changes: Changes<'_>, known: &mut Analysis) {
    for (number, place) in function.places.iter().enumerate() {
        if !place_stays_apart(function, place, changes, known) {
            continue;
        }
        known.places_taken.insert(place.value.0, number);
        known.taken.insert(place.value.0);
        for part in place.read.iter().chain(&place.back).filter(|part| **part != place.value) {
            known.place_parts.insert(part.0, place.value);
            known.taken.insert(part.0);
        }
        note_place_paths(function, place, known);
    }
}

/// The instructions between reading a changed place and starting to write it back, when it's
/// all in one block as the lowering writes it.
fn place_between<'f>(function: &'f Function, place: &ChangedPlace) -> Option<&'f [Inst]> {
    let puts_back = |inst: &InstKind| match inst {
        InstKind::GetField { dest, .. } | InstKind::GetIndex { dest, .. } => place.back.contains(dest),
        InstKind::SetField { object, value, .. } | InstKind::SetIndex { object, value, .. } => {
            *object == place.root && *value == Operand::Local(place.value)
        }
        _ => false,
    };
    let reads_it = |inst: &Inst| matches!(&inst.kind, InstKind::GetField { dest, .. } | InstKind::GetIndex { dest, .. } if *dest == place.value);
    let block = function.blocks.iter().find(|block| block.insts.iter().any(reads_it))?;
    let read_at = block.insts.iter().position(reads_it)?;
    let rest = block.insts.get(read_at + 1..)?;
    let back_at = rest.iter().position(|inst| puts_back(&inst.kind))?;
    // The way back ends with the root written.
    let ends = rest.get(back_at..)?.iter().any(|inst| match &inst.kind {
        InstKind::SetField { object, value: Operand::Local(put), .. }
        | InstKind::SetIndex { object, value: Operand::Local(put), .. } => {
            *object == place.root && (*put == place.value || place.back.contains(put))
        }
        _ => false,
    });
    ends.then(|| rest.get(..back_at)).flatten()
}

/// Whether a call or a built-in changes a place, given it once. What's worked out for the call's
/// other arguments can read the root, but not change it, and owns what it reads, so it can leave
/// taking the place out until just before the call. After the call, nothing reads the root or
/// writes what the place is reached by.
fn place_stays_apart(
    function: &Function,
    place: &ChangedPlace,
    changes: Changes<'_>,
    known: &Analysis,
) -> bool {
    let Some(between) = place_between(function, place) else { return false };
    let held = Operand::Local(place.value);
    let parts: Vec<LocalId> = place.read.iter().chain(&place.back).copied().collect();
    if parts.iter().any(|part| known.writes.get(part.0) != Some(&1))
        || known.reads.get(place.value.0) != Some(&2)
    {
        return false;
    }
    let indexes: Vec<LocalId> = place
        .steps
        .iter()
        .filter_map(|step| match step {
            PlaceStep::Item(Operand::Local(index)) => Some(*index),
            _ => None,
        })
        .collect();
    let mut changed = false;
    for inst in between {
        let (mut read, mut written) = (Vec::new(), Vec::new());
        visit(&inst.kind, |operand| read.push(operand.clone()), |local| written.push(local));
        let reads_local = |local: LocalId| read.contains(&Operand::Local(local));
        let reads_part = parts.iter().any(|part| *part != place.value && reads_local(*part));
        if reads_part
            || written
                .iter()
                .any(|local| *local == place.root || parts.contains(local) || indexes.contains(local))
            || may_change(&inst.kind, &indexes, changes)
            || may_change(&inst.kind, std::slice::from_ref(&place.root), changes)
            || (changed && (reads_local(place.root) || read.contains(&held)))
        {
            return false;
        }
        if read.contains(&held) {
            let as_changed = match &inst.kind {
                InstKind::Call { function: callee, args, .. } => {
                    args.iter().filter(|arg| **arg == held).count() == 1
                        && !args.contains(&Operand::Local(place.root))
                        && changes(callee).iter().any(|at| args.get(*at) == Some(&held))
                }
                InstKind::Builtin { args, changes: true, .. } => {
                    args.first() == Some(&held)
                        && !args.iter().skip(1).any(|arg| *arg == held || *arg == Operand::Local(place.root))
                }
                _ => false,
            };
            if !as_changed {
                return false;
            }
            changed = true;
        }
    }
    changed
}

/// The path from its root to each local read on the way to a place and back.
fn note_place_paths(function: &Function, place: &ChangedPlace, known: &mut Analysis) {
    let mut paths: HashMap<usize, Vec<String>> = HashMap::from([(place.root.0, Vec::new())]);
    let parts = |local: LocalId| place.read.contains(&local) || place.back.contains(&local);
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        let (dest, from, step) = match &inst.kind {
            InstKind::GetField { dest, object: Operand::Local(from), field } if parts(*dest) => {
                (*dest, *from, field.clone())
            }
            InstKind::GetIndex { dest, object: Operand::Local(from), .. } if parts(*dest) => {
                (*dest, *from, ANY_ITEM.to_owned())
            }
            _ => continue,
        };
        if let Some(mut path) = paths.get(&from.0).cloned() {
            path.push(step);
            known.place_paths.insert(dest.0, (place.root, path.clone()));
            paths.insert(dest.0, path);
        }
    }
}

/// Lets a place go back to being copied if what changes it, or anything after that, reads a local
/// leaning on what holds it: what a call is given mustn't change under it. And says which are
/// taken out only when they're needed, as other arguments are worked out first.
fn settle_place_takes(function: &Function, known: &mut Analysis) {
    for place in &function.places {
        if !known.places_taken.contains_key(&place.value.0) {
            continue;
        }
        let Some(between) = place_between(function, place) else { continue };
        let held = Operand::Local(place.value);
        let leans_on_root = |mut local: LocalId| {
            for _ in 0..8 {
                match known.lender(local).or_else(|| known.alias(local)) {
                    Some(from) if from == place.root => return true,
                    Some(from) => local = from,
                    None => break,
                }
            }
            known.alias(place.root) == Some(local)
        };
        let mut leaning = false;
        let mut reached = false;
        let mut first_things = false;
        for inst in between {
            let mut read = Vec::new();
            visit(&inst.kind, |operand| read.push(operand.clone()), |_| {});
            reached |= read.contains(&held);
            first_things |= !reached;
            if reached {
                leaning |= read.iter().any(|operand| {
                    matches!(operand, Operand::Local(local) if *local != place.value && leans_on_root(*local))
                });
            }
        }
        if leaning {
            known.places_taken.remove(&place.value.0);
            known.taken.remove(&place.value.0);
            known.place_paths.remove(&place.value.0);
            for part in place.read.iter().chain(&place.back) {
                known.place_parts.remove(&part.0);
                known.place_paths.remove(&part.0);
                known.taken.remove(&part.0);
            }
        } else if first_things {
            known.places_waiting.insert(place.value.0);
        }
    }
}

/// `root.list[index].field.append(x)` or `list[index].field.append(x)`, which the IR writes as
/// reading the field out, changing it, then reading the item again to put it back: the field is
/// taken, not copied, when nothing else touches the root meanwhile and the index stays the same.
fn find_path_takes(function: &Function, known: &mut Analysis) {
    let steady = |operand: &Operand, known: &Analysis| match operand {
        Operand::Int(_) => true,
        Operand::Local(local) => known.writes.get(local.0).copied().unwrap_or(0) <= 1,
        _ => false,
    };
    for block in &function.blocks {
        let defined = |local: LocalId, before: usize| {
            block.insts.get(..before).unwrap_or_default().iter().rev().find_map(|inst| match &inst.kind {
                InstKind::GetField { dest, object: Operand::Local(from), field } if *dest == local => {
                    Some((Some(field.clone()), *from, None))
                }
                InstKind::GetIndex { dest, object: Operand::Local(from), index } if *dest == local => {
                    Some((None, *from, Some(index.clone())))
                }
                _ => None,
            })
        };
        for (at, inst) in block.insts.iter().enumerate() {
            let InstKind::GetField { dest, object: Operand::Local(item), field } = &inst.kind else {
                continue;
            };
            if known.writes.get(dest.0) != Some(&1) || known.taken.contains(&dest.0) {
                continue;
            }
            let Some((None, list, Some(index))) = defined(*item, at) else { continue };
            // The list is a field of the root, or the root itself.
            let (root, list_path) = match defined(list, at) {
                Some((Some(list_field), root, None)) => (root, vec![list_field]),
                None => (list, Vec::new()),
                Some(_) => continue,
            };
            if !steady(&index, known) || !known.borrowed.contains(&item.0) {
                continue;
            }
            // Then: changes to it, the same place read again, and it put back there.
            let mut again: (Option<LocalId>, Option<LocalId>) = (list_path.is_empty().then_some(root), None);
            let mut put_in_item = false;
            for next in block.insts.get(at + 1..).unwrap_or_default() {
                match &next.kind {
                    InstKind::Builtin { name, args, .. }
                        if name == "list.append"
                            && args.first() == Some(&Operand::Local(*dest))
                            && args.iter().skip(1).all(|arg| *arg != Operand::Local(root)) => {}
                    InstKind::GetField { dest: list2, object: Operand::Local(from), field: name }
                        if *from == root && list_path.first() == Some(name) && again.0.is_none() =>
                    {
                        again.0 = Some(*list2);
                    }
                    InstKind::GetIndex { dest: item2, object: Operand::Local(from), index: at_again }
                        if Some(*from) == again.0 && *at_again == index && again.1.is_none() =>
                    {
                        again.1 = Some(*item2);
                    }
                    InstKind::SetField { object, path, value: Operand::Local(put) }
                        if Some(*object) == again.1
                            && *put == *dest
                            && path.as_slice() == std::slice::from_ref(field) =>
                    {
                        put_in_item = true;
                    }
                    InstKind::SetIndex { object, path, index: at_again, value: Operand::Local(put) }
                        if put_in_item
                            && *object == root
                            && *path == list_path
                            && *at_again == index
                            && Some(*put) == again.1 =>
                    {
                        known.taken.insert(dest.0);
                        known.path_taken.insert(dest.0, (root, list_path.clone(), index.clone()));
                        break;
                    }
                    other if is_quiet(other) && !reads_local(other, root) && !reads_local(other, *dest) => {}
                    _ => break,
                }
            }
        }
    }
}

/// `for mut item in list`: the item is read at the top of the body and written back on every way
/// out of it, which are other blocks, so `find_borrows` doesn't see it. Taking it means what it
/// holds has one owner while the body runs, so changing that changes it in place. The list's
/// place is out of date meanwhile, so nothing may read it on any path to a write-back. A failure
/// the runtime stops on leaves with no write-back, but nothing can catch it and read the list.
fn find_loop_takes(function: &Function, known: &mut Analysis) {
    for (start, block) in function.blocks.iter().enumerate() {
        for (at, inst) in block.insts.iter().enumerate() {
            let InstKind::GetIndex { dest, object: Operand::Local(from), index } = &inst.kind else {
                continue;
            };
            let is_list = matches!(function.locals.get(from.0).map(|local| &local.ty), Some(Type::List(_)));
            if !is_list
                || dest == from
                || known.writes.get(dest.0) != Some(&1)
                || known.taken.contains(&dest.0)
                || known.borrowed.contains(&dest.0)
            {
                continue;
            }
            let Some((root, path)) = written_back_to(function, *dest, index) else { continue };
            if comes_from(function, known, *from, (root, &path))
                && stays_apart(function, known, (start, at + 1), (*dest, *from, index), (root, &path))
            {
                known.taken.insert(dest.0);
                known.items_taken.insert(dest.0, (root, path));
                known.loop_taken.insert(dest.0);
            }
        }
    }
}

/// Where a local read from a list at `index` is written back to, when that's one place.
fn written_back_to(function: &Function, item: LocalId, index: &Operand) -> Option<(LocalId, Vec<String>)> {
    let mut home: Option<(LocalId, &Vec<String>)> = None;
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        if let InstKind::SetIndex { object, path, index: at, value: Operand::Local(put) } = &inst.kind
            && *put == item
        {
            if at != index || home.is_some_and(|(root, known)| root != *object || known != path) {
                return None;
            }
            home = Some((*object, path));
        }
    }
    home.map(|(root, path)| (root, path.clone()))
}

/// Whether a list being read is the one at this place: a loop's copy that's the list itself,
/// read from the local or from one of its fields.
fn comes_from(
    function: &Function,
    known: &Analysis,
    list: LocalId,
    (root, path): (LocalId, &[String]),
) -> bool {
    let whole = known.alias(list).unwrap_or(list);
    match path {
        [] => whole == root,
        [field] => known.writes.get(whole.0) == Some(&1)
            && function.blocks.iter().flat_map(|block| &block.insts).any(|inst| {
                matches!(&inst.kind, InstKind::GetField { dest, object: Operand::Local(from), field: name }
                        if *dest == whole && *from == root && name == field)
            }),
        _ => false,
    }
}

/// Whether, on every path from here, the item gets written back before anything could read the
/// place it came from, and every read of the item is on those paths.
fn stays_apart(
    function: &Function,
    known: &Analysis,
    from: (usize, usize),
    (item, list, index): (LocalId, LocalId, &Operand),
    (root, path): (LocalId, &[String]),
) -> bool {
    let whole = known.alias(list).unwrap_or(list);
    let is_back = |inst: &InstKind| matches!(inst, InstKind::SetIndex { value: Operand::Local(put), .. } if *put == item);
    // Another field of what holds the list is apart from it.
    let other_field = |inst: &InstKind| match (inst, path.first()) {
        (InstKind::GetField { object: Operand::Local(held), field, .. }, Some(first)) => {
            *held == root && field != first
        }
        (
            InstKind::SetField { object, path: at, .. } | InstKind::SetIndex { object, path: at, .. },
            Some(first),
        ) => *object == root && at.first().is_some_and(|field| field != first),
        _ => false,
    };
    let mut reads = 0_u32;
    let mut seen = HashSet::new();
    let mut waiting = vec![from];
    while let Some((at, first)) = waiting.pop() {
        let Some(block) = function.blocks.get(at) else { return false };
        let mut written_back = false;
        for inst in block.insts.get(first..).unwrap_or_default() {
            let (mut read_here, mut written_here) = (Vec::new(), Vec::new());
            visit(&inst.kind, |operand| read_here.push(operand.clone()), |local| written_here.push(local));
            for operand in &read_here {
                if *operand == Operand::Local(item) {
                    reads += 1;
                }
            }
            if is_back(&inst.kind) {
                written_back = true;
                break;
            }
            let reads_home =
                [root, list, whole].iter().any(|held| read_here.contains(&Operand::Local(*held)));
            let writes = |local: LocalId| written_here.contains(&local);
            if (reads_home && !other_field(&inst.kind))
                || writes(root)
                || writes(list)
                || writes(whole)
                || writes(item)
                || matches!(index, Operand::Local(local) if writes(*local))
            {
                return false;
            }
        }
        if written_back {
            continue;
        }
        match &block.end {
            Terminator::Jump(_) | Terminator::Branch { .. } => {
                if matches!(&block.end, Terminator::Branch { cond, .. } if *cond == Operand::Local(item)) {
                    return false;
                }
                for target in block.end.targets() {
                    if seen.insert(target.0) {
                        waiting.push((target.0, 0));
                    }
                }
            }
            Terminator::Unreachable => {}
            // Leaving with the item not written back.
            _ => return false,
        }
    }
    known.reads.get(item.0) == Some(&reads)
}

/// Whether an instruction reads a local.
fn reads_local(inst: &InstKind, local: LocalId) -> bool {
    let mut found = false;
    visit(inst, |operand| found |= *operand == Operand::Local(local), |_| {});
    found
}

/// The blocks of a loop: its body, and what that reaches without going back through its test.
fn loop_blocks(function: &Function, header: usize, body: usize) -> HashSet<usize> {
    let mut found = HashSet::new();
    let mut waiting = vec![body];
    while let Some(at) = waiting.pop() {
        if at != header && found.insert(at) {
            let Some(block) = function.blocks.get(at) else { continue };
            waiting.extend(block.end.targets().into_iter().map(|target| target.0));
        }
    }
    found
}

/// Whether anything in a loop does more to a list than change the item at the loop's count.
fn loop_touches(body: &[&InstKind], list: LocalId, at_count: &dyn Fn(&Operand) -> bool) -> bool {
    body.iter().any(|inst| match inst {
        InstKind::SetIndex { object, path, index, .. } => {
            *object == list && (!path.is_empty() || !at_count(index))
        }
        InstKind::Builtin { name, args, changes, .. } => {
            (*changes || name == "list.append") && args.first() == Some(&Operand::Local(list))
        }
        InstKind::Call { args, .. } => args.contains(&Operand::Local(list)),
        InstKind::Copy { dest, .. } | InstKind::GetIndex { dest, .. } | InstKind::GetField { dest, .. } => {
            *dest == list
        }
        _ => false,
    })
}

/// `for mut item in list` reads from a copy of the list and writes each item back, and a loop
/// over a list that changes only the item it's on can't tell the copy from the list. So the copy
/// can be the list, as long as nothing in the loop does anything else to it.
fn find_aliases(function: &Function, known: &mut Analysis) {
    let insts = || function.blocks.iter().flat_map(|held| &held.insts);
    for (header, block) in function.blocks.iter().enumerate() {
        let Terminator::Branch { cond: Operand::Local(cond), then, .. } = &block.end else { continue };
        let test = block.insts.iter().find_map(|inst| match &inst.kind {
            InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(count), .. } if dest == cond => {
                Some(*count)
            }
            _ => None,
        });
        let Some(count) =
            test.filter(|count| function.counts.iter().any(|held| held.count == *count && !held.down))
        else {
            continue;
        };
        let inside = loop_blocks(function, header, then.0);
        let body: Vec<&InstKind> = inside
            .iter()
            .filter_map(|at| function.blocks.get(*at))
            .flat_map(|held| &held.insts)
            .map(|inst| &inst.kind)
            .collect();
        // The count, or a local that only ever holds a copy of it.
        let writes = known.writes.clone();
        let at_count = |index: &Operand| match index {
            Operand::Local(local) => {
                *local == count
                    || (writes.get(local.0) == Some(&1)
                        && insts().any(|inst| {
                            matches!(&inst.kind, InstKind::Copy { dest, value: Operand::Local(from) }
                                if dest == local && *from == count)
                        }))
            }
            _ => false,
        };
        for inst in insts() {
            let InstKind::Copy { dest: copy, value: Operand::Local(list) } = &inst.kind else { continue };
            let unnamed = function.locals.get(copy.0).is_some_and(|local| local.name.is_none());
            if !unnamed || known.writes.get(copy.0) != Some(&1) {
                continue;
            }
            // Everything that reads the copy: its length, and items at the count in the loop.
            let mut reads = 0;
            for read in insts() {
                match &read.kind {
                    InstKind::Builtin { name, args, .. }
                        if name == "list.len" && args.first() == Some(&Operand::Local(*copy)) =>
                    {
                        reads += 1;
                    }
                    InstKind::GetIndex { object: Operand::Local(from), index, .. }
                        if from == copy && body.contains(&&read.kind) && at_count(index) =>
                    {
                        reads += 1;
                    }
                    _ => {}
                }
            }
            if reads > 0 && known.reads.get(copy.0) == Some(&reads) && !loop_touches(&body, *list, &at_count)
            {
                known.aliases.insert(copy.0, *list);
            }
        }
    }
}

/// `[f(x) for x in range(n)]` is an empty list and a loop of appends.
fn find_reservations(function: &Function, stepped: &HashSet<usize>, known: &mut Analysis) {
    for (at, block) in function.blocks.iter().enumerate() {
        let Terminator::Jump(header) = &block.end else { continue };
        let Some(header) = function.blocks.get(header.0) else { continue };
        let Terminator::Branch { cond: Operand::Local(cond), then, .. } = &header.end else { continue };
        let limit = header.insts.iter().find_map(|inst| match &inst.kind {
            InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(count), rhs }
                if dest == cond && stepped.contains(&count.0) =>
            {
                Some(rhs.clone())
            }
            _ => None,
        });
        let Some(limit) = limit else { continue };
        let Some(body) = function.blocks.get(then.0) else { continue };
        let appended = body.insts.iter().find_map(|inst| match &inst.kind {
            InstKind::Builtin { name, args, .. } if name == "list.append" => match args.first() {
                Some(Operand::Local(list)) => Some(list.0),
                _ => None,
            },
            _ => None,
        });
        let Some(list) = appended else { continue };
        let made_here = block
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::MakeList { dest, items } if dest.0 == list && items.is_empty()));
        if made_here {
            known.reservations.insert(at, (list, limit));
            if fills_with_zeros(function, block, (header, body), list, known) {
                known.zero_filled.insert(list);
            }
        }
    }
}

/// Whether a loop does nothing but append a zero to `list` each turn, counting up from 0 by 1,
/// so that it appends exactly as many as its limit says.
fn fills_with_zeros(
    function: &Function,
    before: &Block,
    (header, body): (&Block, &Block),
    list: usize,
    known: &Analysis,
) -> bool {
    let Terminator::Branch { cond: Operand::Local(cond), .. } = &header.end else { return false };
    let count = header.insts.iter().find_map(|inst| match &inst.kind {
        InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(count), .. } if dest == cond => {
            Some(*count)
        }
        _ => None,
    });
    let Some(count) = count else { return false };
    if !function.counts.iter().any(|found| found.count == count && !found.down) {
        return false;
    }
    let starts_at_zero = before.insts.iter().rev().find_map(|inst| match &inst.kind {
        InstKind::Copy { dest, value } if *dest == count => Some(*value == Operand::Int(0)),
        _ => None,
    });
    if starts_at_zero != Some(true) || header.insts.len() != 1 {
        return false;
    }
    let mut appends = 0;
    for inst in &body.insts {
        match &inst.kind {
            InstKind::Builtin { name, args, .. }
                if name == "list.append"
                    && args.first() == Some(&Operand::Local(LocalId(list)))
                    && matches!(args.get(1), Some(Operand::Int(0) | Operand::Bool(false)))
                    && args.len() == 2 =>
            {
                appends += 1;
            }
            InstKind::Copy { dest, .. } if known.reads.get(dest.0) == Some(&0) => {}
            _ => return false,
        }
    }
    // The turn ends by stepping the count and nothing else.
    let Terminator::Jump(latch) = body.end else { return false };
    appends == 1 && function.blocks.get(latch.0).is_some_and(|latch| latch.insts.len() == 1)
}

impl Analysis {
    pub(crate) fn is_safe(&self, dest: LocalId) -> bool {
        self.safe.contains(&dest.0)
    }

    pub(crate) fn is_nonzero(&self, operand: &Operand) -> bool {
        match operand {
            Operand::Local(local) => self.nonzero.contains(&local.0),
            Operand::Float(value) => *value != 0.0,
            _ => false,
        }
    }

    /// Whether `list[index]` can't be out of range.
    pub(crate) fn inside(&self, list: LocalId, index: &Operand) -> bool {
        let Operand::Local(index) = index else { return false };
        let Some(limit) = self.below.get(&index.0) else { return false };
        self.length_of.get(&limit.0) == Some(&list.0)
    }

    /// Whether reading `from` is the only thing that ever reads what it holds, so a copy can
    /// take over its owner rather than sharing.
    pub(crate) fn moves(&self, from: LocalId) -> bool {
        self.reads.get(from.0) == Some(&1)
            && self.writes.get(from.0) == Some(&1)
            && !self.read_again.contains(&from.0)
    }

    /// Whether the copy at this place in this block is the last anything does with its source.
    pub(crate) fn is_last_copy(&self, block: usize, index: usize) -> bool {
        self.last_copies.contains(&(block, index))
    }

    /// Whether a local is written once and read once.
    pub(crate) fn used_once(&self, local: LocalId) -> bool {
        self.reads.get(local.0) == Some(&1) && self.writes.get(local.0) == Some(&1)
    }

    /// Whether this function changes a list in a local nothing else here comes to hold, so
    /// it's enough to see that it has one owner when the local gets it.
    pub(crate) fn alone_once_checked(&self, local: LocalId) -> bool {
        self.changed.contains(&local.0) && !self.shared.contains(&local.0)
    }

    /// What a local borrows from, if it's borrowing.
    pub(crate) fn lender(&self, local: LocalId) -> Option<LocalId> {
        self.borrowed_from.get(&local.0).copied()
    }

    /// Whether a local looks at a value without owning it.
    pub(crate) fn is_borrowed(&self, local: LocalId) -> bool {
        self.borrowed.contains(&local.0)
    }

    /// Whether a local took a field's value out, to put it back, so its owner moves both ways.
    /// Whether anything here could give a local's value another owner.
    pub(crate) fn is_shared(&self, local: LocalId) -> bool {
        self.shared.contains(&local.0)
    }

    /// The place a local holds while a call changes it, taken out.
    pub(crate) fn place_taken<'f>(&self, function: &'f Function, local: LocalId) -> Option<&'f ChangedPlace> {
        function.places.get(*self.places_taken.get(&local.0)?)
    }

    /// The local holding a place, if this one's read on the way to it or back, or is it.
    pub(crate) fn place_holder(&self, local: LocalId) -> Option<LocalId> {
        if self.places_taken.contains_key(&local.0) {
            return Some(local);
        }
        self.place_parts.get(&local.0).copied()
    }

    /// Whether a place is taken out just before what changes it.
    pub(crate) fn place_waits(&self, local: LocalId) -> bool {
        self.places_waiting.contains(&local.0)
    }

    /// Whether a local is read on the way to a place or back, so it's never read at all.
    pub(crate) fn is_place_part(&self, local: LocalId) -> bool {
        self.place_parts.contains_key(&local.0)
    }

    /// The path from its root to a place's local, for what's known of what's under it.
    pub(crate) fn place_path(&self, local: LocalId) -> Option<&FieldPath> {
        self.place_paths.get(&local.0)
    }

    /// Whether a call here may change a local, through a `mut` parameter.
    pub(crate) fn is_given_mut(&self, local: LocalId) -> bool {
        self.given_mut.contains(&local.0)
    }

    pub(crate) fn is_taken(&self, local: LocalId) -> bool {
        self.taken.contains(&local.0)
    }

    /// The list a local stands for, when it needs no copy of its own.
    pub(crate) fn alias(&self, local: LocalId) -> Option<LocalId> {
        self.aliases.get(&local.0).copied()
    }

    /// Where the list is that a local took an item out of.
    /// Whether a local is a `for mut` item taken out for the whole of the loop's body.
    pub(crate) fn taken_over_loop(&self, local: LocalId) -> bool {
        self.loop_taken.contains(&local.0)
    }

    /// Where a field taken out of a list's item to change goes back: the root, the path to the
    /// list from it, and the index.
    pub(crate) fn path_home(&self, local: LocalId) -> Option<&(LocalId, Vec<String>, Operand)> {
        self.path_taken.get(&local.0)
    }

    /// The local and field a local was read from, if that's its one write.
    pub(crate) fn field_of(&self, local: LocalId) -> Option<&(LocalId, String)> {
        self.field_of.get(&local.0)
    }

    pub(crate) fn item_home(&self, local: LocalId) -> Option<&(LocalId, Vec<String>)> {
        self.items_taken.get(&local.0)
    }

    /// Whether anything writes a local.
    pub(crate) fn written(&self, local: LocalId) -> bool {
        self.writes.get(local.0).is_some_and(|writes| *writes > 0)
    }
}

/// Which locals each block's end, and each block's start, may still read before writing:
/// worked out back from where they're read, round loops until nothing changes.
fn live_after_blocks(function: &Function) -> Vec<HashSet<usize>> {
    let reads_of_end = |end: &Terminator| -> Vec<usize> {
        match end {
            Terminator::Return(Some(Operand::Local(local)))
            | Terminator::Branch { cond: Operand::Local(local), .. }
            | Terminator::NoMatch { value: Operand::Local(local), .. } => vec![local.0],
            _ => Vec::new(),
        }
    };
    let count = function.blocks.len();
    let mut live_in: Vec<HashSet<usize>> = vec![HashSet::new(); count];
    let mut live_out: Vec<HashSet<usize>> = vec![HashSet::new(); count];
    loop {
        let mut changed = false;
        for (at, block) in function.blocks.iter().enumerate().rev() {
            let mut out = HashSet::new();
            for next in block.end.targets() {
                out.extend(live_in.get(next.0).into_iter().flatten().copied());
            }
            let mut live = out.clone();
            live.extend(reads_of_end(&block.end));
            for inst in block.insts.iter().rev() {
                let (mut reads, mut writes) = (Vec::new(), Vec::new());
                visit(&inst.kind, |operand| reads.extend(local_of(operand)), |local| writes.push(local.0));
                for write in writes {
                    live.remove(&write);
                }
                live.extend(reads);
            }
            if live_out.get(at) != Some(&out) || live_in.get(at) != Some(&live) {
                changed = true;
                if let Some(slot) = live_out.get_mut(at) {
                    *slot = out;
                }
                if let Some(slot) = live_in.get_mut(at) {
                    *slot = live;
                }
            }
        }
        if !changed {
            return live_out;
        }
    }
}

fn local_of(operand: &Operand) -> Option<usize> {
    match operand {
        Operand::Local(local) => Some(local.0),
        _ => None,
    }
}

/// Whether nothing changes a map from where a walk of it is asked for, on any way from there to
/// where the walk reads it, round its loop included.
fn walk_leaves_alone(function: &Function, made: LocalId, map: LocalId, read_at: usize) -> bool {
    let mut after = HashMap::new();
    let mut before: HashMap<usize, Vec<usize>> = HashMap::new();
    for (at, block) in function.blocks.iter().enumerate() {
        for next in block.end.targets() {
            before.entry(next.0).or_default().push(at);
        }
        if let Some(index) = block.insts.iter().position(
            |inst| matches!(&inst.kind, InstKind::Builtin { dest: Some(dest), .. } if *dest == made),
        ) {
            after.insert(at, index + 1);
        }
    }
    let Some((&start, &first)) = after.iter().next() else { return false };
    let reach = |from: usize, links: &dyn Fn(usize) -> Vec<usize>| {
        let mut seen = HashSet::from([from]);
        let mut waiting = vec![from];
        while let Some(at) = waiting.pop() {
            for next in links(at) {
                if seen.insert(next) {
                    waiting.push(next);
                }
            }
        }
        seen
    };
    let forward = |at: usize| {
        function
            .blocks
            .get(at)
            .map_or_else(Vec::new, |block| block.end.targets().iter().map(|next| next.0).collect())
    };
    let backward = |at: usize| before.get(&at).cloned().unwrap_or_default();
    let from_start = reach(start, &forward);
    let to_read = reach(read_at, &backward);
    let changes = |inst: &InstKind| {
        let mut written = false;
        visit(inst, |_| {}, |local| written |= local == map);
        written
            || match inst {
                InstKind::SetIndex { object, .. } | InstKind::SetField { object, .. } => *object == map,
                InstKind::Builtin { args, changes: true, .. } => args.first() == Some(&Operand::Local(map)),
                InstKind::Call { args, .. } => args.contains(&Operand::Local(map)),
                _ => false,
            }
    };
    from_start.intersection(&to_read).all(|at| {
        let skipped = if *at == start && !to_read_comes_back(&to_read, &before, start) { first } else { 0 };
        function
            .blocks
            .get(*at)
            .is_some_and(|block| block.insts.iter().skip(skipped).all(|inst| !changes(&inst.kind)))
    })
}

/// Whether the block a walk's asked for in can be reached again on the way to its read, so what
/// comes before the asking counts too.
fn to_read_comes_back(to_read: &HashSet<usize>, before: &HashMap<usize, Vec<usize>>, start: usize) -> bool {
    before.get(&start).is_some_and(|froms| froms.iter().any(|from| to_read.contains(from)))
}

/// `enumerate(list)` where all that's done with what it gives is counting it and reading pairs
/// to take apart right away: neither it nor the pairs are made. A pair's count is where its
/// index points in the list, and its item is the list's there, as nothing here changes the list.
fn find_unmade_enumerates(function: &Function, known: &mut Analysis) {
    let insts: Vec<(usize, usize, &InstKind)> = function
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(at, block)| {
            block.insts.iter().enumerate().map(move |(index, inst)| (at, index, &inst.kind))
        })
        .collect();
    let changed_here = |list: LocalId| {
        known.writes.get(list.0).copied().unwrap_or(0) > 1
            || insts.iter().any(|(_, _, inst)| match inst {
                InstKind::SetIndex { object, .. } | InstKind::SetField { object, .. } => *object == list,
                InstKind::Builtin { args, changes: true, .. } => args.first() == Some(&Operand::Local(list)),
                InstKind::Call { args, .. } => args.contains(&Operand::Local(list)),
                _ => false,
            })
    };
    for (_, _, inst) in &insts {
        let InstKind::Builtin { dest: Some(made), name, args, .. } = inst else { continue };
        let [Operand::Local(list)] = args.as_slice() else { continue };
        // The list, or what it borrows from, stays as it is: a parameter that isn't `mut`, or a
        // local nothing here changes.
        let mut steady = false;
        let mut held = *list;
        for _ in 0..8 {
            if held.0 < function.params {
                steady = !function.mut_params.contains(&held.0);
                break;
            }
            if known.is_taken(held) || known.alias(held).is_some() || changed_here(held) {
                break;
            }
            if let Some(lender) = known.lender(held) {
                held = lender;
            } else {
                steady = !known.is_borrowed(held);
                break;
            }
        }
        let walks = matches!(name.as_str(), "IdMap.items" | "IdMap.values");
        // A loop over a map can't change it, as the checker sees to, so a walk only needs nothing
        // to change it from when this was asked for until its loop's done. Something else can
        // reach a map through a `shared` handle, so unless it's known steady, the local holds an
        // owner of its own for the walk, which makes such a change copy it.
        let holds_it = !known.is_borrowed(*list) && !known.is_taken(*list) && known.alias(*list).is_none();
        let steady = steady || (walks && holds_it);
        if !(name == "enumerate" || walks) || !steady || known.writes.get(made.0) != Some(&1) {
            continue;
        }
        // The result and the copies of it, each written once.
        let mut group = vec![*made];
        let mut grown = true;
        while grown {
            grown = false;
            for (_, _, inst) in &insts {
                if let InstKind::Copy { dest, value: Operand::Local(from) } = inst
                    && group.contains(from)
                    && !group.contains(dest)
                    && known.writes.get(dest.0) == Some(&1)
                {
                    group.push(*dest);
                    grown = true;
                }
            }
        }
        let mut pairs = Vec::new();
        let mut fine = true;
        for &(at, index, inst) in &insts {
            let mut read = Vec::new();
            visit(inst, |operand| read.push(operand.clone()), |_| {});
            let reads_group =
                read.iter().any(|operand| matches!(operand, Operand::Local(local) if group.contains(local)));
            if !reads_group {
                continue;
            }
            match inst {
                InstKind::Copy { dest, .. } if group.contains(dest) => {}
                InstKind::Builtin { name, args, .. } if name == "list.len" && args.len() == 1 => {}
                InstKind::GetIndex { dest, object: Operand::Local(_), index: pair_at }
                    if known.writes.get(dest.0) == Some(&1) =>
                {
                    pairs.push((*dest, pair_at.clone(), at, index));
                }
                _ => fine = false,
            }
        }
        // A walk goes through the slots in order, once for each count of one loop.
        if walks {
            let counted = |at: &Operand| {
                function.counts.iter().any(|count| !count.down && *at == Operand::Local(count.count))
            };
            fine &= matches!(pairs.as_slice(), [(_, at, _, _)] if counted(at));
            fine &= pairs
                .iter()
                .all(|(_, _, walked_at, _)| walk_leaves_alone(function, *made, *list, *walked_at));
        }
        // Each pair is only taken apart, after it's read, in its block.
        for (pair, _, at, index) in pairs.iter().filter(|_| name != "IdMap.values") {
            let Some(block) = function.blocks.get(*at) else { continue };
            let later = block.insts.get(index + 1..).unwrap_or_default();
            let mut uses = 0;
            for next in later {
                let mut read = Vec::new();
                visit(&next.kind, |operand| read.push(operand.clone()), |_| {});
                if read.contains(&Operand::Local(*pair)) {
                    match &next.kind {
                        InstKind::GetTupleItem { value, at: 0 | 1, .. }
                            if *value == Operand::Local(*pair) =>
                        {
                            uses += 1;
                        }
                        _ => fine = false,
                    }
                }
            }
            if uses != known.reads.get(pair.0).copied().unwrap_or(0) as usize {
                fine = false;
            }
        }
        if fine && walks {
            for local in &group {
                known.map_walks.insert(local.0, *list);
            }
            for (item, _, _, _) in pairs {
                known.walk_items.insert(item.0, (*list, name == "IdMap.items"));
            }
        } else if fine {
            for local in &group {
                known.enumerated.insert(local.0, *list);
            }
            for (pair, pair_at, _, _) in pairs {
                known.pairs.insert(pair.0, (*list, pair_at));
            }
        }
    }
}

/// Copies whose source nothing reads after them, before writing it again, and that nothing
/// leans on: borrows, aliases, or what's taken out of it.
fn find_last_copies(function: &Function, known: &mut Analysis) {
    let mut leaned_on = HashSet::new();
    for local in 0..function.locals.len() {
        let id = LocalId(local);
        leaned_on.extend(known.lender(id).map(|from| from.0));
        leaned_on.extend(known.alias(id).map(|from| from.0));
        leaned_on.extend(known.field_of(id).map(|(from, _)| from.0));
        leaned_on.extend(known.item_home(id).map(|(from, _)| from.0));
        leaned_on.extend(known.path_home(id).map(|(from, _, _)| from.0));
    }
    let live_out = live_after_blocks(function);
    for (at, block) in function.blocks.iter().enumerate() {
        let mut live = live_out.get(at).cloned().unwrap_or_default();
        if let Terminator::Return(Some(Operand::Local(local)))
        | Terminator::Branch { cond: Operand::Local(local), .. }
        | Terminator::NoMatch { value: Operand::Local(local), .. } = &block.end
        {
            live.insert(local.0);
        }
        for (index, inst) in block.insts.iter().enumerate().rev() {
            if let InstKind::Copy { dest, value: Operand::Local(from) } = &inst.kind
                && dest != from
                && !live.contains(&from.0)
                && !leaned_on.contains(&from.0)
                && from.0 >= function.params
            {
                known.last_copies.insert((at, index));
            }
            let (mut reads, mut writes) = (Vec::new(), Vec::new());
            visit(&inst.kind, |operand| reads.extend(local_of(operand)), |local| writes.push(local.0));
            for write in writes {
                live.remove(&write);
            }
            live.extend(reads);
        }
    }
}

/// Whether anything a function does itself can fail. Calling something counts, since a call is
/// where the stack is checked.
fn fails_itself(function: &Function, known: &Analysis) -> bool {
    let inst_fails = |inst: &InstKind| match inst {
        InstKind::Binary { dest, op, lhs, rhs } => match (function.type_of(lhs), op) {
            (Some(Type::Int), BinOp::Add | BinOp::Sub | BinOp::Mul) => !known.is_safe(*dest),
            (Some(Type::Int), BinOp::FloorDiv | BinOp::Mod | BinOp::Div) => !matches!(rhs, Operand::Int(1..)),
            (Some(Type::Float), BinOp::Div | BinOp::FloorDiv | BinOp::Mod) => !known.is_nonzero(rhs),
            // Items of a list may order by a type's own `<`, which can fail.
            (Some(Type::Int), BinOp::Pow | BinOp::ShiftLeft | BinOp::ShiftRight)
            | (Some(Type::List(_)), BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq) => true,
            _ => false,
        },
        InstKind::Unary { dest, op: UnaryOp::Neg, value } => {
            function.type_of(value) == Some(Type::Int) && !known.is_safe(*dest)
        }
        // A capability's method runs in the runtime, which can fail it, as when it's narrowed.
        InstKind::Builtin { args, .. }
            if args.first().and_then(|first| function.type_of(first)).is_some_and(
                |ty| matches!(&ty, Type::Named(name) if fennec_layout::Cap::named(name).is_some()),
            ) =>
        {
            true
        }
        InstKind::Builtin { name, .. } if name.starts_with("crypto.") => true,
        InstKind::Builtin { name, .. } if name.starts_with("json.") || name.starts_with("toml.") => true,
        InstKind::Builtin { name, .. } => {
            matches!(
                name.as_str(),
                "math.sqrt"
                    | "Buffer.pop"
                    | "Buffer.insert"
                    | "Buffer.remove"
                    | "json.dumps"
                    | "json.loads"
                    | "str.code"
                    | "str.split"
                    | "int.abs"
                    | "sum"
                    | "sum.start"
                    | "list.pop"
                    | "list.insert"
                    | "sorted.key"
                    | "list.remove_where"
                    | "result.expect"
                    | "dict.pop"
                    | "random.random_int"
                    | "random_choice"
                    | "random.random_choice"
                    | "int"
                    | "math.floor"
                    | "math.ceil"
                    | "round"
                    | "range"
                    | "max"
                    | "min"
                    | "max.key"
                    | "min.key"
                    | "list.remove"
                    | "Regex.replace"
                    | "Regex.split"
                    | "Regex.find_all"
                    | "Regex.search"
                    | "Regex.match"
                    | "Regex.fullmatch"
                    // A range's body can fail on any thread, and the call passes it up.
                    | "parallel.for"
                    | "parallel.map"
                    | "parallel.sum"
            )
        }
        InstKind::GetIndex { object: Operand::Local(list), index, .. } => !known.inside(*list, index),
        InstKind::SetIndex { object, path, index, .. } => !path.is_empty() || !known.inside(*object, index),
        InstKind::GetIndex { .. }
        | InstKind::Call { .. }
        | InstKind::Format { .. }
        | InstKind::Slice { .. }
        | InstKind::CallClosure { .. }
        | InstKind::Resume { .. }
        | InstKind::LockAll { .. }
        | InstKind::Constant { .. } => true,
        _ => false,
    };
    function.blocks.iter().any(|block| {
        matches!(block.end, Terminator::Stop { .. } | Terminator::NoMatch { .. })
            || block.insts.iter().any(|inst| inst_fails(&inst.kind))
    })
}

/// For each function, which parameters it might give another owner: copy, store, return, or
/// pass to something that might. Worked out over every function at once, as calls go both ways.
fn parameters_kept<'p>(functions: &HashMap<&'p str, &'p Function>) -> HashMap<&'p str, Vec<bool>> {
    let mut kept: HashMap<&str, Vec<bool>> =
        functions.iter().map(|(name, function)| (*name, vec![false; function.params])).collect();
    loop {
        let mut changed = false;
        for (name, function) in functions {
            for at in 0..function.params {
                if kept.get(name).and_then(|params| params.get(at)).copied().unwrap_or(true) {
                    continue;
                }
                if keeps_parameter(function, at, &kept) {
                    if let Some(slot) = kept.get_mut(name).and_then(|params| params.get_mut(at)) {
                        *slot = true;
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

/// Whether a builtin can give back what it works on, rather than only looking at it or
/// changing it in place.
pub(crate) fn gives_back(name: &str) -> bool {
    matches!(
        name,
        "list"
            | "parallel"
            | "str"
            | "str.strip"
            | "str.lstrip"
            | "str.rstrip"
            | "sorted"
            | "reversed"
            | "copy"
    )
}

/// Whether a function might give its parameter at `at` another owner, given what's known of
/// the functions it calls.
fn keeps_parameter(function: &Function, at: usize, kept: &HashMap<&str, Vec<bool>>) -> bool {
    let me = Operand::Local(LocalId(at));
    let ends = function
        .blocks
        .iter()
        .any(|block| matches!(&block.end, Terminator::Return(Some(value)) if *value == me));
    ends || function.blocks.iter().flat_map(|block| &block.insts).any(|inst| match &inst.kind {
        // Read by a builtin as what it works on, it's only looked at or changed in place,
        // except by those that can give back what they're given.
        InstKind::Builtin { name, args, .. } => {
            let gives = gives_back(name);
            args.iter().enumerate().any(|(position, arg)| *arg == me && (position > 0 || gives))
        }
        InstKind::Call { function: callee, args, .. } => args.iter().enumerate().any(|(position, arg)| {
            *arg == me
                && kept
                    .get(callee.as_str())
                    .is_none_or(|params| params.get(position).copied().unwrap_or(true))
        }),
        InstKind::GetField { .. }
        | InstKind::GetIndex { .. }
        | InstKind::GetTupleItem { .. }
        | InstKind::GetCaseField { .. }
        | InstKind::IsCase { .. }
        | InstKind::IsNone { .. }
        | InstKind::Binary { .. }
        | InstKind::Unary { .. }
        | InstKind::SetField { .. }
        | InstKind::SetIndex { .. } => {
            let mut stored = false;
            if let InstKind::SetField { value, .. } | InstKind::SetIndex { value, .. } = &inst.kind {
                stored = *value == me;
            }
            stored
        }
        other => {
            let mut read = false;
            visit(other, |operand| read |= *operand == me, |_| {});
            read
        }
    })
}

pub(crate) fn whole(program: &Program) -> Whole<'_> {
    let every = || program.functions.iter().chain(&program.constants);
    let functions: HashMap<&str, &Function> =
        every().map(|function| (function.name.as_str(), function)).collect();
    let changes = |callee: &str| functions.get(callee).map_or_else(Vec::new, |f| f.mut_params.clone());
    let kept = parameters_kept(&functions);
    let keeps = |callee: &str, at: usize| {
        kept.get(callee).is_none_or(|params| params.get(at).copied().unwrap_or(true))
    };
    let analyses: HashMap<&str, Analysis> =
        every().map(|function| (function.name.as_str(), analyze(function, &changes, &keeps))).collect();
    let mut generators = HashMap::new();
    for def in &program.generators {
        generators.insert(def.name.as_str(), def);
        generators.insert(def.resume.as_str(), def);
        if let Some(abandon) = &def.abandon {
            generators.insert(abandon.as_str(), def);
        }
    }
    // A generator's functions are called through its block, so all the same way: with the flag.
    let mut can_fail: HashSet<&str> = every()
        .filter(|function| {
            analyses.get(function.name.as_str()).is_none_or(|known| fails_itself(function, known))
        })
        .map(|function| function.name.as_str())
        .collect();
    for def in &program.generators {
        can_fail.insert(def.resume.as_str());
        can_fail.extend(def.abandon.as_deref());
    }
    let settles = summarized(&functions, &analyses, Tracked::Owners);
    let lengths = summarized(&functions, &analyses, Tracked::Lengths);
    let needs = needed(program, &functions, &analyses);
    let builders = crate::builders::builders(program, &functions);
    let constants = every()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.kind {
            InstKind::Constant { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let on_workers = crate::parallel::run_on_workers(program);
    Whole {
        functions,
        analyses,
        can_fail,
        generators,
        settles,
        lengths,
        builders,
        needs,
        constants,
        on_workers,
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    const TYPES: &str = "struct Unit:\n    tags: list[int]\n    hp: int\n\nstruct World:\n    units: list[Unit]\n    log: list[int]\n\nerror Bad:\n    why: str\n\ndef total(units: list[Unit]) -> int:\n    units.len()\n\ndef size(world: World) -> int:\n    world.log.len()\n\n";

    /// Whether the `unit` of a function called `go` is taken out for its loop's whole body.
    fn taken(body: &str) -> Result<bool, String> {
        let source = format!("{TYPES}{body}");
        let module = fennec_syntax::parse(&source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let known = analyze(function, &|_| Vec::new(), &|_, _| false);
        let at = function.locals.iter().position(|local| local.name.as_deref() == Some("unit"));
        Ok(known.taken_over_loop(LocalId(at.ok_or("no `unit`")?)))
    }

    #[test]
    fn a_for_mut_item_is_taken_on_every_way_out_of_its_body() -> Result<(), String> {
        for body in [
            "def go(mut units: list[Unit]):\n    for mut unit in units:\n        unit.tags.append(1)\n",
            "def go(mut units: list[Unit]):\n    for mut unit in units:\n        if unit.hp == 0:\n            continue\n        unit.tags.append(1)\n        if unit.hp == 1:\n            break\n        unit.hp = 2\n",
            "def go(mut units: list[Unit]) -> int:\n    for mut unit in units:\n        unit.tags.append(1)\n        if unit.hp == 1:\n            return unit.tags.len()\n    0\n",
            "def go(mut units: list[Unit]) -> int fails Bad:\n    for mut unit in units:\n        unit.tags.append(1)\n        if unit.hp == 1:\n            fail Bad(\"no\")\n    0\n",
            "def go(mut world: World):\n    for mut unit in world.units:\n        unit.tags.append(1)\n",
            "def go(mut world: World):\n    for mut unit in world.units:\n        world.log.append(unit.hp)\n        unit.tags.append(1)\n",
        ] {
            assert!(taken(body)?, "not taken: {body}");
        }
        Ok(())
    }

    /// How many locals of a function called `go` are taken out of where they came from.
    fn taken_locals(body: &str) -> Result<usize, String> {
        let source = format!("struct Note:\n    text: str\n    hp: int\n\n{body}");
        let module = fennec_syntax::parse(&source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let known = analyze(function, &|_| Vec::new(), &|_, _| false);
        Ok((0..function.locals.len()).filter(|at| known.is_taken(LocalId(*at))).count())
    }

    /// Only `+=` on a text in a field takes it out: added to in any other way, or for a number,
    /// what's in the field has to last until the new value is put there.
    #[test]
    fn a_text_growing_in_a_field_is_taken_out_of_it() -> Result<(), String> {
        let grows = "def go(mut note: Note, more: str):\n    note.text += more\n";
        assert_eq!(taken_locals(grows)?, 1);
        let makes = "def go(mut note: Note, more: str):\n    note.text = note.text + more\n";
        assert_eq!(taken_locals(makes)?, 0);
        let number = "def go(mut note: Note):\n    note.hp += 1\n";
        assert_eq!(taken_locals(number)?, 0);
        Ok(())
    }

    /// The list's place is out of date while the body runs, so a body that could read it isn't
    /// taken: reading a stale address gives a wrong answer and no failure.
    #[test]
    fn a_for_mut_item_is_not_taken_when_the_body_can_read_its_list() -> Result<(), String> {
        for body in [
            "def go(mut units: list[Unit]):\n    for mut unit in units:\n        unit.tags.append(units[0].hp)\n",
            "def go(mut units: list[Unit]):\n    for mut unit in units:\n        unit.hp += units.len()\n",
            "def go(mut units: list[Unit]):\n    for mut unit in units:\n        unit.hp += total(units)\n",
            "def go(mut world: World):\n    for mut unit in world.units:\n        unit.hp += world.units.len()\n",
            "def go(mut world: World):\n    for mut unit in world.units:\n        unit.hp += size(world)\n",
            "def go(units: list[Unit]) -> int:\n    mut found = 0\n    for unit in units:\n        found += unit.hp\n    found\n",
        ] {
            assert!(!taken(body)?, "taken: {body}");
        }
        Ok(())
    }
}
