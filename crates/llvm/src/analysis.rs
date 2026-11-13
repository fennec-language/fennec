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
    /// Lists that may be one a lock's rollback log has marked, only appended to, extended or popped
    /// here: a marked list never reads as having one owner, so each change there asks, and none is
    /// let off asking by an earlier one (see `lists::MARKED` in the runtime).
    maybe_marked: HashSet<usize>,
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
    /// Names an item copied out with `mut` is changed under, by the local it was read into
    /// first, which still holds what was taken: a build that checks proofs checks it goes back
    /// where it came from.
    put_back_reads: HashMap<usize, LocalId>,
    /// `for ref` items taken out of a `Table`'s slot: the local or field the `Table` is in,
    /// which has to be alone before the item is taken.
    slot_homes: HashMap<usize, (LocalId, Vec<String>)>,
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
    /// For each of those, the loop's count, which is how many it has appended at each turn.
    pub(crate) zero_counts: HashMap<usize, LocalId>,
    /// Lists such a loop appends to once a turn, at most as many times as the room made for
    /// them, and that nothing else in the loop touches: an append never needs more room, and
    /// the list is never shared while it's filled.
    pub(crate) room_enough: HashSet<usize>,
    /// Of those, the lists whose loop leaves only to the block after it, by the header, each
    /// with that block: the length is kept in a local while the loop fills it, and written to
    /// the list there. Any other way out is a failure, which ends the program.
    pub(crate) length_kept: HashMap<usize, usize>,
    /// Copies, by block and place in it, whose source nothing reads again or leans on: the
    /// copy takes over its owner.
    last_copies: HashSet<(usize, usize)>,
    /// Reads of a result's value or error that are the last anything does with the result, so
    /// what's read can be handed over, the result let go of straight after.
    last_reads: HashSet<(usize, usize)>,
    /// Fields a struct is made with, by block, place in it and local, that nothing reads again or
    /// leans on: the struct takes over the local's owner.
    last_made: HashSet<(usize, usize, usize)>,
    /// `enumerate(list)` results, and copies of them, never made: each with the list.
    pub(crate) enumerated: HashMap<usize, LocalId>,
    /// `map.items()` or `map.values()` where all that's done with what it gives is going through
    /// it in order: it's never made, as the loop walks the map's slots. Which map.
    pub(crate) map_walks: HashMap<usize, LocalId>,
    /// What such a loop reads each time round: which map, and whether it's a pair to take apart.
    pub(crate) walk_items: HashMap<usize, (LocalId, bool)>,
    /// Of those, the ones reading a dict's keys rather than its values.
    pub(crate) walk_keys: HashSet<usize>,
    /// Pairs read from those, never made: the list, and the index the pair is at.
    pub(crate) pairs: HashMap<usize, (LocalId, Operand)>,
    /// Reads of a list's item, by block and place in it, that a guard proves in range: see
    /// `find_guarded_reads`.
    guarded_reads: HashSet<(usize, usize)>,
    /// The int parameters every call gives a known range, with it (see `param_ranges`). A build
    /// that checks proofs checks each on the way in.
    pub(crate) param_ranges: Vec<(usize, Range)>,
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
    /// The list indexes proven not negative, so they need no way back from the end.
    pub(crate) non_negative: crate::signs::NonNegative,
    /// The field paths of `mut` parameters each function's callers make alone before calling.
    pub(crate) needs: HashMap<&'p str, Vec<FieldPath>>,
    /// Those a function only may change through, which a caller looks at and leaves as they are.
    pub(crate) may_need: HashMap<&'p str, Vec<FieldPath>>,
    /// The functions that give a constant's value, which keep it after the first time.
    pub(crate) constants: HashSet<&'p str>,
    /// The functions that can run on a thread `parallel` uses, which everything else can't.
    pub(crate) on_workers: HashSet<&'p str>,
    /// Whether the program has any `shared` type.
    pub(crate) cells: bool,
    /// The functions that can run as a parked task, which count their loops' turns.
    pub(crate) tasks: HashSet<&'p str>,
    /// Whether its closures are blocks in the runtime's table, since it keeps one where the
    /// runtime reads (see `closures`).
    pub(crate) runtime_closures: bool,
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

fn range_of(facts: &Facts, multi: &HashMap<usize, Range>, operand: &Operand, block: usize) -> Range {
    match operand {
        Operand::Local(local) => {
            facts.range_in(*local, block).or_else(|| multi.get(&local.0).copied()).unwrap_or(FULL)
        }
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
        // A lock's word is in the cell, so a handle has to last until it's given back.
        InstKind::Lock { handle, .. } | InstKind::Unlock { handle } => read(handle),
        // A crash's cleanup has to last until a crash could run it.
        InstKind::CrashCleanup { cleanup } | InstKind::CrashCleanupDone { cleanup } => read(cleanup),
        InstKind::LockAll { handles } => handles.iter().for_each(|(handle, _)| read(handle)),
        InstKind::UnlockAll { handles } => handles.iter().for_each(&mut read),
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
        | InstKind::MakeMember { dest, value, .. }
        | InstKind::IsMember { dest, value, .. }
        | InstKind::GetMember { dest, value, .. }
        | InstKind::IsCase { dest, value, .. }
        | InstKind::GetCaseField { dest, value, .. }
        | InstKind::MakeErr { dest, error: value }
        | InstKind::IsErr { dest, value }
        | InstKind::GetOk { dest, value }
        | InstKind::GetErr { dest, value }
        | InstKind::IsError { dest, value, .. }
        | InstKind::AsError { dest, value } => {
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
        | InstKind::ConstantList { dest, items }
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
    }
}

/// Whether a builtin keeps what it's given after its receiver, so the given value has another owner.
fn keeps_an_argument(name: &str) -> bool {
    matches!(
        name,
        "list.append"
            | "list.insert"
            | "set.add"
            | "Buffer.push"
            | "Buffer.insert"
            | "Table.add"
            | "dict.setdefault"
    )
}

/// What the facts leave to say about one instruction.
fn note(
    function: &Function,
    (facts, multi): (&mut Facts, &HashMap<usize, Range>),
    known: &mut Analysis,
    stepped: &HashSet<usize>,
    (block, inst): (usize, &InstKind),
) {
    match inst {
        InstKind::Copy { dest, value } => {
            facts.note_write(*dest, range_of(facts, multi, value, block));
            if let Operand::Local(from) = value {
                // Only a local written once stays what it was given: any other can become 0 after.
                if known.nonzero.contains(&from.0) && facts.holds_one_value(*dest) {
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
            let (a, b) = (range_of(facts, multi, lhs, block), range_of(facts, multi, rhs, block));
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
            let range = range_of(facts, multi, value, block);
            if function.type_of(value) == Some(Type::Int) && range.0 > i64::MIN {
                known.safe.insert(dest.0);
                facts.note_write(*dest, (-range.1, -range.0));
            }
        }
        InstKind::Builtin { dest: Some(dest), name, args, .. } => match name.as_str() {
            "float" => {
                let (low, high) = args.first().map_or(FULL, |arg| range_of(facts, multi, arg, block));
                if (low > 0 || high < 0) && facts.holds_one_value(*dest) {
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
            // Letting go of what a local holds at a lock's edge (`shared.drop`) isn't a use of it:
            // a local that only borrows owns nothing to let go of, so it can still borrow.
            if matches!(&inst.kind, InstKind::Builtin { name, .. } if name == "shared.drop") {
                reads.clear();
            }
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

/// Notes the lists that may be one a lock's rollback log has marked, where each change should ask:
/// `ref` parameters, what's read out of a field or an item, what a call changes in place, and copies
/// of any of them, when every change here to the list is an append, an extend or a pop, which the
/// log notes and leaves in place. A list changed any other way here is better copied once on the way
/// in, as it is when its first check copies a marked one, than asked about at every change.
fn find_maybe_marked(function: &Function, known: &mut Analysis) {
    let mut maybe: HashSet<usize> = function.mut_params.iter().copied().collect();
    maybe.extend(function.places.iter().map(|place| place.value.0));
    let mut grew = true;
    while grew {
        grew = false;
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            let dest = match &inst.kind {
                InstKind::GetField { dest, .. } | InstKind::GetIndex { dest, .. } => Some(*dest),
                InstKind::Copy { dest, value: Operand::Local(from) } if maybe.contains(&from.0) => {
                    Some(*dest)
                }
                _ => None,
            };
            if let Some(dest) = dest {
                grew |= maybe.insert(dest.0);
            }
        }
    }
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        match &inst.kind {
            InstKind::Builtin { name, args, changes, .. } if *changes || name == "list.append" => {
                let noted = matches!(name.as_str(), "list.append" | "list.extend")
                    || (name == "list.pop" && args.len() == 1);
                if !noted && let Some(Operand::Local(list)) = args.first() {
                    maybe.remove(&list.0);
                }
            }
            InstKind::SetIndex { object, .. } => {
                maybe.remove(&object.0);
            }
            _ => {}
        }
    }
    known.maybe_marked = maybe;
}

fn analyze(
    function: &Function,
    changes: &dyn Fn(&str) -> Vec<usize>,
    keeps: &dyn Fn(&str, usize) -> bool,
    params: &[Range],
) -> Analysis {
    let mut known = Analysis::empty(function.locals.len());
    known.param_ranges = given_ranges(function, params);
    let grown = count_uses(function, (changes, keeps), &mut known);
    find_maybe_marked(function, &mut known);
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
            | InstKind::MakeMember { value: Operand::Local(from), .. }
            | InstKind::GetMember { value: Operand::Local(from), .. }
                if !known.moves(*from) =>
            {
                known.shared.insert(from.0);
            }
            InstKind::Make { fields: items, .. }
            | InstKind::MakeCase { fields: items, .. }
            | InstKind::MakeList { items, .. }
            | InstKind::ConstantList { items, .. }
            | InstKind::MakeTuple { items, .. }
            | InstKind::MakeSet { items, .. }
            | InstKind::MakeClosure { captures: items, .. } => {
                for item in items {
                    if let Operand::Local(local) = item {
                        known.shared.insert(local.0);
                    }
                }
            }
            InstKind::MakeDict { pairs, .. } => {
                for item in pairs.iter().flat_map(|(key, value)| [key, value]) {
                    if let Operand::Local(local) = item {
                        known.shared.insert(local.0);
                    }
                }
            }
            InstKind::MakeGenerator { args, .. } => {
                for (_, value) in args {
                    if let Operand::Local(local) = value {
                        known.shared.insert(local.0);
                    }
                }
            }
            InstKind::SetField { value: Operand::Local(local), .. }
            | InstKind::SetIndex { value: Operand::Local(local), .. }
            | InstKind::SetSlot { value: Operand::Local(local), .. }
            | InstKind::MakeOk { value: Some(Operand::Local(local)), .. }
            | InstKind::MakeErr { error: Operand::Local(local), .. } => {
                known.shared.insert(local.0);
            }
            InstKind::Builtin { name, args, .. } if keeps_an_argument(name) => {
                for arg in args.iter().skip(1) {
                    if let Operand::Local(local) = arg {
                        known.shared.insert(local.0);
                    }
                }
            }
            _ => {}
        }
    }
    let stepped: HashSet<usize> = function.counts.iter().map(|count| count.count.0).collect();
    let (facts, multi) = ranges(function, changes, &mut known, &stepped);
    find_guarded_steps(function, (&facts, &multi), &mut known);
    // A length only says something about a list that keeps it.
    let writes = known.writes.clone();
    let params = function.params;
    known.length_of.retain(|_, list| {
        let written = writes.get(*list).copied().unwrap_or(2);
        !grown.contains(list) && written == u32::from(*list >= params)
    });
    find_guarded_reads(function, (&facts, &multi), &mut known);
    known.below.retain(|local, _| facts.range_in(LocalId(*local), 0).is_some_and(|range| range.0 >= 0));
    find_reservations(function, &stepped, &mut known);
    find_slice_keys(function, &mut known);
    find_unmade_enumerates(function, &mut known);
    find_counts_inside(function, &mut known);
    known
}

/// One side of a comparison: a local, or a number written out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Local(usize),
    Int(i64),
}

/// A comparison known on the way to a place, `x op y`, with `op` one of `<`, `<=`, `>`, `>=`: so
/// far as it holds outright, or whenever a bool local is true, or whenever it's false.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Guard {
    Holds(usize, u8, Side),
    IfTrue(usize, usize, u8, Side),
    IfFalse(usize, usize, u8, Side),
}

const LESS: u8 = 0;
const AT_MOST: u8 = 1;
const MORE: u8 = 2;
const AT_LEAST: u8 = 3;

fn relation(op: BinOp) -> Option<u8> {
    Some(match op {
        BinOp::Lt => LESS,
        BinOp::LtEq => AT_MOST,
        BinOp::Gt => MORE,
        BinOp::GtEq => AT_LEAST,
        _ => return None,
    })
}

/// `y op x` for `x op y`, and what's true when `x op y` isn't.
fn turned(relation: u8) -> u8 {
    match relation {
        LESS => MORE,
        AT_MOST => AT_LEAST,
        MORE => LESS,
        _ => AT_MOST,
    }
}

fn denied(relation: u8) -> u8 {
    match relation {
        LESS => AT_LEAST,
        AT_MOST => MORE,
        MORE => AT_MOST,
        _ => LESS,
    }
}

impl Guard {
    /// Whether it says anything about a local.
    /// The local this says something about: the one on the left of its comparison.
    fn about(&self) -> usize {
        match *self {
            Guard::Holds(x, _, _) | Guard::IfTrue(_, x, _, _) | Guard::IfFalse(_, x, _, _) => x,
        }
    }

    fn mentions(&self, local: usize) -> bool {
        let (flag, x, y) = match *self {
            Guard::Holds(x, _, y) => (None, x, y),
            Guard::IfTrue(flag, x, _, y) | Guard::IfFalse(flag, x, _, y) => (Some(flag), x, y),
        };
        flag == Some(local) || x == local || y == Side::Local(local)
    }
}

/// The comparisons a compare's result says, each way round: `a < b` says `a < b` and `b > a`.
fn compared(function: &Function, op: BinOp, lhs: &Operand, rhs: &Operand) -> Vec<(usize, u8, Side)> {
    let side = |operand: &Operand| match operand {
        Operand::Local(local) if function.locals.get(local.0).is_some_and(|held| held.ty == Type::Int) => {
            Some(Side::Local(local.0))
        }
        Operand::Int(value) => Some(Side::Int(*value)),
        _ => None,
    };
    let (Some(relation), Some(a), Some(b)) = (relation(op), side(lhs), side(rhs)) else { return Vec::new() };
    let mut found = Vec::new();
    if let Side::Local(x) = a {
        found.push((x, relation, b));
    }
    if let Side::Local(y) = b {
        found.push((y, turned(relation), a));
    }
    found
}

/// What's known after an instruction, from what was known before it. `range` says what's known
/// about a local anywhere it's read, for keeping what a comparison said about one side when the
/// other side is written: `x > y`, with `y` written, still says `x` is above the lowest `y` can be.
fn guarded_after(
    function: &Function,
    known: &mut std::collections::BTreeSet<Guard>,
    inst: &InstKind,
    (range, stepped, given_mut): (&dyn Fn(usize) -> Range, &HashSet<usize>, &HashSet<usize>),
) {
    let mut written = Vec::new();
    visit(inst, |_| {}, |local| written.push(local.0));
    // A call can write what it's given by `ref`, which nothing here shows.
    if let InstKind::Call { args, .. } = inst {
        written.extend(args.iter().filter_map(|arg| match arg {
            Operand::Local(local) if given_mut.contains(&local.0) => Some(local.0),
            _ => None,
        }));
    }
    let before = known.clone();
    let mut kept = Vec::new();
    for guard in known.iter() {
        if let Guard::Holds(x, relation, Side::Local(y)) = *guard
            && written.contains(&y)
            && !written.contains(&x)
        {
            let (low, high) = range(y);
            kept.push(match relation {
                LESS => Guard::Holds(x, AT_MOST, Side::Int(high.saturating_sub(1))),
                AT_MOST => Guard::Holds(x, AT_MOST, Side::Int(high)),
                MORE => Guard::Holds(x, AT_LEAST, Side::Int(low.saturating_add(1))),
                _ => Guard::Holds(x, AT_LEAST, Side::Int(low)),
            });
        }
    }
    known.retain(|guard| !written.iter().any(|local| guard.mentions(*local)));
    known.extend(kept);
    for flag in written
        .iter()
        .copied()
        .filter(|at| function.locals.get(*at).is_some_and(|held| held.ty == Type::Bool))
    {
        let mut new = Vec::new();
        // A bool written while a comparison holds says it either way.
        for guard in known.iter() {
            if let Guard::Holds(x, relation, y) = *guard {
                new.push(Guard::IfTrue(flag, x, relation, y));
                new.push(Guard::IfFalse(flag, x, relation, y));
            }
        }
        match inst {
            InstKind::Binary { op, lhs, rhs, .. } => {
                for (x, relation, y) in compared(function, *op, lhs, rhs) {
                    if x != flag && y != Side::Local(flag) {
                        new.push(Guard::IfTrue(flag, x, relation, y));
                        new.push(Guard::IfFalse(flag, x, denied(relation), y));
                    }
                }
            }
            // A copy of a bool says what that one said.
            InstKind::Copy { value: Operand::Local(from), .. } => {
                for guard in &before {
                    match *guard {
                        Guard::IfTrue(at, x, relation, y) if at == from.0 && !written.contains(&x) => {
                            new.push(Guard::IfTrue(flag, x, relation, y));
                        }
                        Guard::IfFalse(at, x, relation, y) if at == from.0 && !written.contains(&x) => {
                            new.push(Guard::IfFalse(flag, x, relation, y));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        known.extend(
            new.into_iter()
                .filter(|guard| !written.iter().any(|local| *local != flag && guard.mentions(*local)))
                .filter(|guard| stepped.contains(&guard.about())),
        );
    }
    known.retain(|guard| stepped.contains(&guard.about()));
}

/// The most comparisons a block's way in may carry before `find_guarded_steps` gives up on
/// its function, so working it out stays small, however many comparisons the function makes.
const MOST_GUARDS: usize = 64;

/// Steps made safe by a comparison on the way to them: `x + 1` where `x < y` holds can't pass the
/// highest int, whatever `y` is, and `x - 1` where `x > y` holds can't pass the lowest. What
/// holds is worked out forward over the blocks: a branch on a compare says it on the way it
/// goes, a bool says what the compare it copies said, and writing a local forgets what was
/// known about it. Where ways in meet, only what holds on all of them is kept.
/// Reads a guard proves in range: after `while at + K <= n:`, where `n` is a list's length and
/// `at` is never negative, the block the guard leads to reads `items[at + j]` for any `j` from 0
/// to `K - 1` in range, and `at + j` can't overflow, until something there changes `at`, `n` or
/// the list. That's how a loop taking a list `K` items a turn reads them, as `math.fixed_sum`
/// does, and with no check between them, LLVM can read and add them as vectors.
fn find_guarded_reads(
    function: &Function,
    (facts, multi): (&Facts, &HashMap<usize, Range>),
    known: &mut Analysis,
) {
    // Where each local is written, by the instruction writing it, for the ones written once.
    let mut made: HashMap<usize, &InstKind> = HashMap::new();
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        visit(
            &inst.kind,
            |_| {},
            |local| {
                made.insert(local.0, &inst.kind);
            },
        );
    }
    // A local a call can write by `ref` isn't written once, whatever it says here.
    let once = |local: usize| known.writes.get(local) == Some(&1) && !known.given_mut.contains(&local);
    let mut ways_in: HashMap<usize, usize> = HashMap::new();
    for block in &function.blocks {
        for next in block.end.targets() {
            *ways_in.entry(next.0).or_default() += 1;
        }
    }
    for block in &function.blocks {
        let Terminator::Branch { cond: Operand::Local(flag), then, .. } = &block.end else { continue };
        if ways_in.get(&then.0) != Some(&1) || !once(flag.0) {
            continue;
        }
        // `flag = t <= n`, `t = at + K`, with `n` a list's length.
        let Some(InstKind::Binary {
            op: BinOp::LtEq,
            lhs: Operand::Local(sum),
            rhs: Operand::Local(length),
            ..
        }) = made.get(&flag.0)
        else {
            continue;
        };
        let Some(&list) = known.length_of.get(&length.0) else { continue };
        if !once(sum.0) {
            continue;
        }
        let Some(InstKind::Binary {
            op: BinOp::Add, lhs: Operand::Local(at), rhs: Operand::Int(step), ..
        }) = made.get(&sum.0)
        else {
            continue;
        };
        if *step <= 0
            || (range_of(facts, multi, &Operand::Local(*at), then.0).0 < 0
                && !never_negative(function, *at, &known.given_mut))
        {
            continue;
        }
        let Some(body) = function.blocks.get(then.0) else { continue };
        let mut near: HashSet<usize> = HashSet::from([at.0]);
        for (index, inst) in body.insts.iter().enumerate() {
            match &inst.kind {
                // `at + j`, which can't overflow, as `at + K` didn't.
                InstKind::Binary {
                    dest,
                    op: BinOp::Add,
                    lhs: Operand::Local(from),
                    rhs: Operand::Int(j),
                } if from == at && (0..*step).contains(j) && once(dest.0) => {
                    near.insert(dest.0);
                    known.safe.insert(dest.0);
                }
                InstKind::GetIndex {
                    object: Operand::Local(read), index: Operand::Local(position), ..
                } if read.0 == list && near.contains(&position.0) => {
                    known.guarded_reads.insert((then.0, index));
                }
                _ => {}
            }
            let mut changes = false;
            visit(&inst.kind, |_| {}, |local| changes |= local == *at || local == *length || local.0 == list);
            if changes || matches!(inst.kind, InstKind::Call { .. }) {
                break;
            }
        }
    }
}

/// Whether a local is never below 0: each write gives it a number from 0 up, or adds one to what
/// it holds, which can't wrap round, as an add that overflows stops the program.
fn never_negative(function: &Function, local: LocalId, given_mut: &HashSet<usize>) -> bool {
    if local.0 < function.params || given_mut.contains(&local.0) {
        return false;
    }
    let insts: Vec<&InstKind> =
        function.blocks.iter().flat_map(|block| &block.insts).map(|inst| &inst.kind).collect();
    // A temporary holding `local + c`, with `c` from 0 up.
    let grown = |from: &LocalId| {
        insts.iter().any(|inst| {
            matches!(inst, InstKind::Binary { dest, op: BinOp::Add, lhs: Operand::Local(base), rhs: Operand::Int(c) }
                if dest == from && *base == local && *c >= 0)
        })
    };
    let mut writes = 0;
    for inst in &insts {
        let mut writes_it = false;
        visit(inst, |_| {}, |written| writes_it |= written == local);
        if !writes_it {
            continue;
        }
        writes += 1;
        let fine = match inst {
            InstKind::Copy { value: Operand::Int(value), .. } => *value >= 0,
            InstKind::Copy { value: Operand::Local(from), .. } => grown(from),
            InstKind::Binary { op: BinOp::Add, lhs: Operand::Local(base), rhs: Operand::Int(c), .. } => {
                *base == local && *c >= 0
            }
            _ => false,
        };
        if !fine {
            return false;
        }
    }
    writes > 0
}

fn find_guarded_steps(
    function: &Function,
    (facts, multi): (&Facts, &HashMap<usize, Range>),
    known: &mut Analysis,
) {
    use std::collections::BTreeSet;
    let steps = |inst: &InstKind| {
        matches!(inst, InstKind::Binary { dest, op: BinOp::Add | BinOp::Sub | BinOp::Mul, lhs, .. }
            if function.type_of(lhs) == Some(Type::Int) && !known.is_safe(*dest))
    };
    if !function.blocks.iter().flat_map(|block| &block.insts).any(|inst| steps(&inst.kind)) {
        return;
    }
    // Only what's said about a local a step reads can prove the step, so nothing else is kept:
    // a function with many comparisons and bools would otherwise carry them all everywhere.
    let stepped: HashSet<usize> = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter(|inst| steps(&inst.kind))
        .flat_map(|inst| match &inst.kind {
            InstKind::Binary { lhs, rhs, .. } => {
                [lhs, rhs].into_iter().filter_map(local_of).collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect();
    let count = function.blocks.len();
    // What holds on each way into each block, once that way's been worked out.
    let mut coming: Vec<HashMap<usize, BTreeSet<Guard>>> = vec![HashMap::new(); count];
    let mut entered: Vec<Option<BTreeSet<Guard>>> = vec![None; count];
    let mut settled = false;
    for _ in 0..64 {
        let mut changed = false;
        for (at, block) in function.blocks.iter().enumerate() {
            let state = if at == 0 {
                Some(BTreeSet::new())
            } else {
                coming.get(at).and_then(|ways| {
                    ways.values().cloned().reduce(|a, b| a.intersection(&b).copied().collect())
                })
            };
            let Some(mut state) = state else { continue };
            // However it's kept small, what's known stays bounded: past this, no step here is
            // proved by it, and every check stays.
            if state.len() > MOST_GUARDS {
                return;
            }
            if let Some(slot) = entered.get_mut(at) {
                *slot = Some(state.clone());
            }
            let range = |local: usize| range_of(facts, multi, &Operand::Local(LocalId(local)), at);
            for inst in &block.insts {
                guarded_after(function, &mut state, &inst.kind, (&range, &stepped, &known.given_mut));
            }
            let outs: Vec<(usize, BTreeSet<Guard>)> = match &block.end {
                Terminator::Branch { cond: Operand::Local(flag), then, otherwise } => {
                    let holds = |when_true: bool| {
                        let mut out = state.clone();
                        for guard in &state {
                            match *guard {
                                Guard::IfTrue(at, x, relation, y) if when_true && at == flag.0 => {
                                    out.insert(Guard::Holds(x, relation, y));
                                }
                                Guard::IfFalse(at, x, relation, y) if !when_true && at == flag.0 => {
                                    out.insert(Guard::Holds(x, relation, y));
                                }
                                _ => {}
                            }
                        }
                        out
                    };
                    if then == otherwise {
                        vec![(then.0, state.clone())]
                    } else {
                        vec![(then.0, holds(true)), (otherwise.0, holds(false))]
                    }
                }
                end => end.targets().into_iter().map(|target| (target.0, state.clone())).collect(),
            };
            for (target, out) in outs {
                if let Some(ways) = coming.get_mut(target)
                    && ways.get(&at) != Some(&out)
                {
                    ways.insert(at, out);
                    changed = true;
                }
            }
        }
        if !changed {
            settled = true;
            break;
        }
    }
    // What was worked out partway may say more than holds.
    if !settled {
        return;
    }
    let bounded = |state: &BTreeSet<Guard>, operand: &Operand, block: usize| -> Range {
        let mut range = range_of(facts, multi, operand, block);
        let Operand::Local(local) = operand else { return range };
        for guard in state {
            let Guard::Holds(x, relation, y) = *guard else { continue };
            if x != local.0 {
                continue;
            }
            let other = match y {
                Side::Int(value) => (value, value),
                Side::Local(at) => range_of(facts, multi, &Operand::Local(LocalId(at)), block),
            };
            match relation {
                LESS => range.1 = range.1.min(other.1.saturating_sub(1)),
                AT_MOST => range.1 = range.1.min(other.1),
                MORE => range.0 = range.0.max(other.0.saturating_add(1)),
                _ => range.0 = range.0.max(other.0),
            }
        }
        range
    };
    // Whether each sum, difference or product is proved, by the local it's written to: what's
    // safe goes by that local, so one written twice is safe only if both are.
    let mut proved: HashMap<usize, bool> = HashMap::new();
    for (at, block) in function.blocks.iter().enumerate() {
        let reached = entered.get(at).cloned().flatten();
        let unreached = reached.is_none();
        let mut state = reached.unwrap_or_default();
        let range = |local: usize| range_of(facts, multi, &Operand::Local(LocalId(local)), at);
        for inst in &block.insts {
            if let InstKind::Binary { dest, op: op @ (BinOp::Add | BinOp::Sub | BinOp::Mul), lhs, rhs } =
                &inst.kind
                && function.type_of(lhs) == Some(Type::Int)
            {
                // A block nothing reaches holds nothing to check.
                let holds = unreached
                    || known.is_safe(*dest)
                    || exact(*op, bounded(&state, lhs, at), bounded(&state, rhs, at)).is_some_and(
                        |(low, high)| low >= i128::from(i64::MIN) && high <= i128::from(i64::MAX),
                    );
                let all = proved.entry(dest.0).or_insert(true);
                *all &= holds;
            }
            guarded_after(function, &mut state, &inst.kind, (&range, &stepped, &known.given_mut));
        }
    }
    known.safe.extend(proved.into_iter().filter(|(_, all)| *all).map(|(dest, _)| dest));
}

/// What's known about each int local written more than once: every value written to it is in
/// the range, given that every such local read is in its own. It's found by guessing from what
/// each write gives with nothing known about them, then checking the guess holds, widening it
/// when it doesn't, and leaving out what still doesn't settle. `seed = (seed * k + c) % m`
/// stays below `m`, so the product that makes it can't overflow.
fn written_ranges(
    function: &Function,
    changes: &dyn Fn(&str) -> Vec<usize>,
    known: &Analysis,
    stepped: &HashSet<usize>,
) -> HashMap<usize, Range> {
    let candidates: HashSet<usize> = function
        .locals
        .iter()
        .enumerate()
        .filter(|(at, local)| {
            *at >= function.params
                && local.ty == Type::Int
                && known.writes.get(*at).is_some_and(|written| *written >= 2)
                && !stepped.contains(at)
                && !known.given_mut.contains(at)
        })
        .map(|(at, _)| at)
        .collect();
    if candidates.is_empty() {
        return HashMap::new();
    }
    let mut guess: HashMap<usize, Range> = HashMap::new();
    for _ in 0..6 {
        let given = writes_under(function, changes, (stepped, &known.param_ranges), &candidates, &guess);
        // A write of something not known at all leaves its local out.
        let next: HashMap<usize, Range> = given.into_iter().filter(|(_, range)| *range != FULL).collect();
        let held = next.iter().all(|(local, range)| {
            guess.get(local).is_some_and(|guessed| guessed.0 <= range.0 && range.1 <= guessed.1)
        });
        if held && next.len() == guess.len() {
            return guess;
        }
        // Widened, every write the guess didn't hold goes in.
        guess = next
            .into_iter()
            .map(|(local, range)| match guess.get(&local) {
                Some(old) => (local, (old.0.min(range.0), old.1.max(range.1))),
                None => (local, range),
            })
            .collect();
    }
    HashMap::new()
}

/// The parameters given a range, as `analyze` keeps them: each int one nothing in the function
/// writes, with what's known of it, where that's anything.
fn given_ranges(function: &Function, params: &[Range]) -> Vec<(usize, Range)> {
    params
        .iter()
        .copied()
        .enumerate()
        .take(function.params)
        .filter(|(at, range)| {
            *range != FULL
                && function.locals.get(*at).is_some_and(|local| local.ty == Type::Int)
                && !function.mut_params.contains(at)
        })
        .collect()
}

/// What's known of a function's ints: the facts, with its parameters' ranges in, and the ranges
/// of ints written more than once. A count stepped once a turn of a loop is bounded by the loop's
/// turns, which the facts say, so finding one notes everything again with it known.
fn ranges(
    function: &Function,
    changes: &dyn Fn(&str) -> Vec<usize>,
    known: &mut Analysis,
    stepped: &HashSet<usize>,
) -> (Facts, HashMap<usize, Range>) {
    let mut multi = written_ranges(function, changes, known, stepped);
    let mut facts = noted(function, changes, known, stepped, &multi);
    let once_a_turn = stepped_once_a_turn(function, known, &facts);
    if !once_a_turn.is_empty() {
        multi.extend(once_a_turn);
        facts = noted(function, changes, known, stepped, &multi);
    }
    (facts, multi)
}

/// The facts of a function, with what each instruction says noted.
fn noted(
    function: &Function,
    changes: &dyn Fn(&str) -> Vec<usize>,
    known: &mut Analysis,
    stepped: &HashSet<usize>,
    multi: &HashMap<usize, Range>,
) -> Facts {
    let mut facts = Facts::of(function, changes);
    for &(at, range) in &known.param_ranges {
        facts.note_param(LocalId(at), range);
    }
    for (block, written) in function.blocks.iter().enumerate() {
        for inst in &written.insts {
            note(function, (&mut facts, multi), known, stepped, (block, &inst.kind));
        }
    }
    facts
}

/// Where a counted loop tests its count: the block, and the blocks it goes to while the count
/// is below its limit, and after.
fn count_test(function: &Function, count: &fennec_ir::Count) -> Option<(usize, usize, usize)> {
    let held = Operand::Local(count.count);
    let at = function.blocks.iter().position(|block| {
        let Terminator::Branch { cond: Operand::Local(cond), .. } = &block.end else { return false };
        block.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Binary { dest, op: BinOp::Lt, lhs, rhs: Operand::Local(stop) }
                if dest == cond && *lhs == held && *stop == count.stop)
        })
    })?;
    match function.blocks.get(at).map(|block| &block.end) {
        Some(Terminator::Branch { then, otherwise, .. }) => Some((at, then.0, otherwise.0)),
        _ => None,
    }
}

/// The blocks reachable from `from`'s successors, leaving out the edge `cut` (from, to).
fn reached(function: &Function, from: usize, cut: (usize, usize)) -> HashSet<usize> {
    let next = |at: usize| {
        function.blocks.get(at).map_or_else(Vec::new, |block| {
            block.end.targets().iter().map(|target| target.0).filter(|to| (at, *to) != cut).collect()
        })
    };
    let mut seen = HashSet::new();
    let mut waiting = next(from);
    while let Some(at) = waiting.pop() {
        if seen.insert(at) {
            waiting.extend(next(at));
        }
    }
    seen
}

/// Ints set to a number once, before a counted loop, and stepped by a number once a turn of it,
/// written nowhere else: each stays between its start and where as many steps as the loop has
/// turns take it, like `mut j = 0` and `j += 1` beside `for x in v`. The loop mustn't run again,
/// as it would inside another, and the step mustn't be in a loop of its own inside it.
fn stepped_once_a_turn(function: &Function, known: &Analysis, facts: &Facts) -> HashMap<usize, Range> {
    let mut found = HashMap::new();
    for (local, info) in function.locals.iter().enumerate().skip(function.params) {
        if info.ty != Type::Int
            || known.writes.get(local) != Some(&2)
            || known.given_mut.contains(&local)
            || function.counts.iter().any(|count| count.count.0 == local)
        {
            continue;
        }
        let (mut start, mut step, mut other) = (None, None, false);
        for (at, block) in function.blocks.iter().enumerate() {
            for inst in &block.insts {
                let mut writes_it = false;
                visit(&inst.kind, |_| {}, |written| writes_it |= written.0 == local);
                if !writes_it {
                    continue;
                }
                match &inst.kind {
                    InstKind::Copy { value: Operand::Int(first), .. } if start.is_none() => {
                        start = Some((at, *first));
                    }
                    InstKind::Binary { op, lhs: Operand::Local(from), rhs: Operand::Int(by), .. }
                        if from.0 == local && *by != 0 && step.is_none() =>
                    {
                        match op {
                            BinOp::Add => step = Some((at, *by)),
                            BinOp::Sub if *by != i64::MIN => step = Some((at, -*by)),
                            _ => other = true,
                        }
                    }
                    _ => other = true,
                }
            }
        }
        let (Some((start_at, first)), Some((step_at, by)), false) = (start, step, other) else { continue };
        for count in function.counts.iter().filter(|count| !count.down) {
            let Some((test, body, _)) = count_test(function, count) else { continue };
            // The block that steps the count and goes back to the test.
            let back = function.blocks.iter().position(|block| {
                matches!(&block.end, Terminator::Jump(to) if to.0 == test)
                    && block.insts.iter().any(|inst| {
                        matches!(&inst.kind, InstKind::Binary { dest, op: BinOp::Add, lhs: Operand::Local(from), rhs: Operand::Int(1) }
                            if *dest == count.count && *from == count.count)
                    })
            });
            let Some(back) = back else { continue };
            let cut = (back, test);
            let from_test = reached(function, test, cut);
            // The loop runs once: nothing but its own step comes back to its test. The start
            // runs once, before it, and the step once a turn, inside it.
            let in_body = reached(function, body, (test, body));
            let once = !from_test.contains(&test)
                && !reached(function, start_at, (usize::MAX, usize::MAX)).contains(&start_at)
                && !from_test.contains(&start_at)
                && start_at != test
                && (step_at == body || in_body.contains(&step_at))
                && reached(function, step_at, cut).contains(&back)
                && !reached(function, step_at, cut).contains(&step_at);
            if !once {
                continue;
            }
            let Some((low, high)) = facts.range_in(count.count, body) else { continue };
            let turns = (i128::from(high) - i128::from(low) + 1).max(0);
            let (first, by) = (i128::from(first), i128::from(by));
            let ends = [first, first + by * turns, 0];
            let (Some(least), Some(most)) = (ends.iter().min(), ends.iter().max()) else { continue };
            if let (Ok(least), Ok(most)) = (i64::try_from(*least), i64::try_from(*most)) {
                found.insert(local, (least, most));
            }
            break;
        }
    }
    found
}

/// The range of every value written to each candidate, with each candidate read taken to be in
/// its guessed range, or anything at all where there's none.
fn writes_under(
    function: &Function,
    changes: &dyn Fn(&str) -> Vec<usize>,
    (stepped, params): (&HashSet<usize>, &[(usize, Range)]),
    candidates: &HashSet<usize>,
    guess: &HashMap<usize, Range>,
) -> HashMap<usize, Range> {
    let mut facts = Facts::of(function, changes);
    for &(at, range) in params {
        facts.note_param(LocalId(at), range);
    }
    let mut scratch = Analysis::empty(function.locals.len());
    let mut given: HashMap<usize, Range> = HashMap::new();
    for (block, written) in function.blocks.iter().enumerate() {
        for inst in &written.insts {
            let mut dests = Vec::new();
            visit(&inst.kind, |_| {}, |local| dests.push(local));
            for dest in dests.into_iter().filter(|dest| candidates.contains(&dest.0)) {
                let range = write_range(function, (&facts, guess), &inst.kind, block);
                let union = given.get(&dest.0).map_or(range, |old| (old.0.min(range.0), old.1.max(range.1)));
                given.insert(dest.0, union);
            }
            note(function, (&mut facts, guess), &mut scratch, stepped, (block, &inst.kind));
        }
    }
    given
}

/// The range of the int an instruction writes, as `note` works it out, or anything at all.
fn write_range(
    function: &Function,
    (facts, multi): (&Facts, &HashMap<usize, Range>),
    inst: &InstKind,
    block: usize,
) -> Range {
    match inst {
        InstKind::Copy { value, .. } => range_of(facts, multi, value, block),
        InstKind::Binary { op, lhs, rhs, .. } if function.type_of(lhs) == Some(Type::Int) => {
            let (a, b) = (range_of(facts, multi, lhs, block), range_of(facts, multi, rhs, block));
            if let Some((low, high)) = exact(*op, a, b)
                && low >= i128::from(i64::MIN)
                && high <= i128::from(i64::MAX)
            {
                return (clamp(low), clamp(high));
            }
            match (op, rhs) {
                (BinOp::FloorDiv, Operand::Int(by @ 1..)) => (a.0.div_euclid(*by), a.1.div_euclid(*by)),
                (BinOp::Mod, Operand::Int(by @ 1..)) => (0, by - 1),
                _ => FULL,
            }
        }
        InstKind::Unary { op: UnaryOp::Neg, value, .. } => {
            let range = range_of(facts, multi, value, block);
            if range.0 > i64::MIN { (-range.1, -range.0) } else { FULL }
        }
        InstKind::Builtin { name, .. } if name == "list.len" => (0, MOST_ITEMS),
        _ => FULL,
    }
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
    #[expect(
        clippy::iter_over_hash_type,
        reason = "each local is checked alone, and what's found goes into a set"
    )]
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
fn may_change(
    inst: &InstKind,
    lenders: &[LocalId],
    changes: &dyn Fn(&str) -> Vec<usize>,
    roots: &Roots,
) -> bool {
    let held = |operand: &Operand| matches!(operand, Operand::Local(local) if lenders.contains(local));
    let mut written = false;
    visit(inst, |_| {}, |local| written |= lenders.contains(&local));
    written
        || match inst {
            InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } => lenders.contains(object),
            InstKind::Builtin { args, changes: true, .. } => args.first().is_some_and(held),
            InstKind::Call { .. } => !leaves_alone(inst, lenders, changes, roots),
            InstKind::Lock { .. }
            | InstKind::Unlock { .. }
            | InstKind::CrashCleanup { .. }
            | InstKind::CrashCleanupDone { .. } => true,
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

/// A read that may borrow: where it's made, what it's read from, everything that has to stay as
/// it is while it's live, and the local at the root of those if it isn't a parameter.
struct Candidate {
    defined: (usize, usize),
    source: LocalId,
    lenders: Vec<LocalId>,
    root: Option<LocalId>,
}

/// Finds more reads of a field or item that need no owner of their own: read any number of
/// times, in any block, but only looked at, while nothing could change what they came from.
/// What they came from has to outlive them: a parameter, or a local still read after each
/// place they're in use, so it can't have been given away or let go before then; or borrowed
/// from one of those.
fn find_live_borrows(function: &Function, changes: &dyn Fn(&str) -> Vec<usize>, known: &mut Analysis) {
    let is_param = |local: LocalId| local.0 < function.params;
    let roots = roots(function, known);
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
                let root = copied.root;
                candidates.insert(dest.0, Candidate { defined: (at, index), source: *from, lenders, root });
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
            let id_map = matches!(source_ty, Some(Type::Table(_)));
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
            let mut root = None;
            while let Some(&last) = lenders.last() {
                if is_param(last) {
                    break;
                }
                match known.borrowed_from.get(&last.0) {
                    Some(lender) if known.borrowed.contains(&last.0) => lenders.push(*lender),
                    // A variable that nothing takes out to change: it lasts while it's still to
                    // be read, which is checked below. Not a temporary, which some lists' loops
                    // read through without owning what's in them.
                    _ if function.locals.get(last.0).is_some_and(|local| local.name.is_some())
                        && owns_where_it_is(known, last) =>
                    {
                        root = Some(last);
                        break;
                    }
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
                candidates.insert(dest.0, Candidate { defined: (at, index), source, lenders, root });
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
            candidates.retain(|dest, Candidate { defined, lenders, root, .. }| {
                // Borrowed from a local, which has to be read again after anywhere this is in
                // use: until then it's neither given away nor let go.
                if let Some(root) = root
                    && (live.contains(dest) || reads.contains(dest))
                    && !live.contains(&root.0)
                {
                    return false;
                }
                // Copied to a local borrowing it in turn, like the list a loop goes over.
                let copied = matches!(&inst.kind, InstKind::Copy { dest: to, value: Operand::Local(from) }
                    if from.0 == *dest && copies.contains(&to.0));
                if reads.contains(dest)
                    && !copied
                    && !reads_without_owning(&inst.kind, LocalId(*dest), changes)
                {
                    return false;
                }
                // Nor changed by what reads it last, like a call given both it and the place it
                // was read from to change.
                let changing = *defined != (at, index) && may_change(&inst.kind, lenders, changes, &roots);
                !(changing && (live.contains(dest) || reads.contains(dest)))
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
    #[expect(clippy::iter_over_hash_type, reason = "what's found goes into a set and a map")]
    for (dest, Candidate { source, .. }) in candidates {
        known.borrowed.insert(dest);
        known.borrowed_from.insert(dest, source);
    }
}

/// Whether a local holds its value itself, as far as is known when borrows are found: not
/// borrowed, nor taken out of somewhere to change and put back, nor on the way to such a place.
fn owns_where_it_is(known: &Analysis, local: LocalId) -> bool {
    let at = &local.0;
    !known.borrowed.contains(at)
        && !known.taken.contains(at)
        && !known.items_taken.contains_key(at)
        && !known.places_taken.contains_key(at)
        && !known.place_parts.contains_key(at)
}

/// Whether a call leaves these locals as they are: only what's given `mut` can change, and
/// nothing else holds a local's value. What's given `mut` changes what it was read out of too: a
/// place read again from the same local, `xs[i]` for a borrow of `xs[i].tags`, is the same block.
fn leaves_alone(
    inst: &InstKind,
    lenders: &[LocalId],
    changes: &dyn Fn(&str) -> Vec<usize>,
    roots: &Roots,
) -> bool {
    let InstKind::Call { dest, function, args } = inst else { return false };
    if dest.is_some_and(|dest| lenders.contains(&dest)) {
        return false;
    }
    let lent_from: HashSet<usize> = lenders.iter().map(|lender| root_of(roots, lender.0)).collect();
    changes(function).iter().all(|at| match args.get(*at) {
        Some(Operand::Local(held)) => !lenders.contains(held) && !lent_from.contains(&root_of(roots, held.0)),
        _ => true,
    })
}

/// For each local read out of another, by a field, an item, a case's field or a copy, that local.
pub(crate) type Roots = HashMap<usize, usize>;

/// What each local of `function` was read out of, where it's written once.
fn roots(function: &Function, known: &Analysis) -> Roots {
    let mut from = Roots::new();
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        let (dest, source) = match &inst.kind {
            InstKind::GetField { dest, object: Operand::Local(source), .. }
            | InstKind::GetIndex { dest, object: Operand::Local(source), .. }
            | InstKind::GetCaseField { dest, value: Operand::Local(source), .. }
            | InstKind::Copy { dest, value: Operand::Local(source) } => (dest.0, source.0),
            _ => continue,
        };
        if known.writes.get(dest) == Some(&1) && dest != source {
            from.insert(dest, source);
        }
    }
    from
}

/// The local `local` was read out of at the start, following `roots` back.
fn root_of(roots: &Roots, local: usize) -> usize {
    let mut at = local;
    // Each step goes to a local written once before, so this ends; the bound is for safety.
    for _ in 0..roots.len() {
        match roots.get(&at) {
            Some(source) if *source != local => at = *source,
            _ => break,
        }
    }
    at
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
                        | "intrinsics.text_byte"
                        | "intrinsics.text_size"
                        | "intrinsics.text_part"
                        | "intrinsics.text_plain_end"
                        | "intrinsics.text_escape_at"
                        | "intrinsics.bytes_text"
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
    let roots = roots(function, known);
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
                    if !is_quiet(&next.kind) && !leaves_alone(&next.kind, &lenders, changes, &roots) {
                        break;
                    }
                }
            }
            // A list's, an array's, a `Table`'s or a dict's item, by position, `Id` or key: each
            // is read from what holds it made alone first, and a dict's value from its own block
            // made alone too. A `Table`'s or a dict's item may be changed by a builtin or a call
            // given it once too, as `held[key].append(n)` and `heal(ref units[id])` are, which
            // the IR notes as a changed place only for a list.
            let by_key = matches!(
                function.locals.get(source.0).map(|local| &local.ty),
                Some(Type::Dict(..) | Type::Table(_))
            );
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
                        InstKind::Builtin { args, changes: true, .. }
                            if by_key
                                && args.first() == Some(&Operand::Local(dest))
                                && !args.iter().skip(1).any(|arg| {
                                    *arg == Operand::Local(dest) || *arg == Operand::Local(source)
                                }) => {}
                        InstKind::Call { function: callee, args, .. }
                            if by_key
                                && args.iter().filter(|arg| **arg == Operand::Local(dest)).count() == 1
                                && changes(callee)
                                    .iter()
                                    .any(|at| args.get(*at) == Some(&Operand::Local(dest)))
                                && !args.contains(&Operand::Local(source)) => {}
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
                        if name.starts_with("Table.")
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
                    | InstKind::ConstantList { .. }
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
            || may_change(&inst.kind, &indexes, changes, &Roots::new())
            || may_change(&inst.kind, std::slice::from_ref(&place.root), changes, &Roots::new())
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
        // A number or a string written out: a list's position, or a dict's key.
        Operand::Int(_) | Operand::Str(_) => true,
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
            // A list's item, a `Table`'s or a dict's value is in a place this can reach: an
            // array's isn't.
            if !matches!(
                function.type_of(&Operand::Local(list)),
                Some(Type::List(_) | Type::Table(_) | Type::Dict(..))
            ) {
                continue;
            }
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
            // A list's item, or an array's: an array kept in place is read and written where it
            // is, and one in a block is made alone first, as a list is.
            let is_list = matches!(
                function.locals.get(from.0).map(|local| &local.ty),
                Some(Type::List(_) | Type::Array(..))
            );
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
    find_slot_takes(function, known);
    find_put_back_takes(function, known);
    find_slot_borrows(function, known);
}

/// `for item in table` whose body only looks inside the item, reading its fields, while nothing
/// in the loop changes the `Table`: the item is borrowed from its slot rather than given an owner
/// of its own and let go of again each time round.
fn find_slot_borrows(function: &Function, known: &mut Analysis) {
    for (start, block) in function.blocks.iter().enumerate() {
        for inst in &block.insts {
            let InstKind::Builtin { dest: Some(dest), name, args, .. } = &inst.kind else { continue };
            let [Operand::Local(from), Operand::Local(slot)] = args.as_slice() else { continue };
            // A `Table` read from a field is read again each time round and let go of: borrowing
            // from it would keep that read, an owner of the `Table`, until the item's last use,
            // and a change to the `Table` after the loop would then copy it.
            if name != "Table.at_slot"
                || known.writes.get(dest.0) != Some(&1)
                || known.taken.contains(&dest.0)
                || known.borrowed.contains(&dest.0)
                || known.field_of(*from).is_some()
            {
                continue;
            }
            // The loop counting the slots: its blocks, from the test of its count.
            let Some(count) = function.counts.iter().find(|count| count.count == *slot) else { continue };
            let header = function.blocks.iter().position(|held| {
                let Terminator::Branch { cond: Operand::Local(cond), .. } = &held.end else { return false };
                held.insts.iter().any(|test| {
                    matches!(&test.kind, InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(lhs), rhs: Operand::Local(stop) }
                        if dest == cond && lhs == slot && *stop == count.stop)
                })
            });
            let Some(header) = header else { continue };
            let Some(Terminator::Branch { then, .. }) = function.blocks.get(header).map(|held| &held.end)
            else {
                continue;
            };
            let body = loop_blocks(function, header, then.0);
            if !body.contains(&start) {
                continue;
            }
            // What the `Table` is read from: itself, what it was copied from, or the local a field
            // of which it is, however many steps back.
            let mut sources = vec![*from];
            let mut at = 0;
            while let Some(held) = sources.get(at).copied() {
                at += 1;
                if sources.len() > 8 {
                    break;
                }
                if let Some((root, _)) = known.field_of(held) {
                    sources.push(*root);
                }
                for copied in function.blocks.iter().flat_map(|block| &block.insts) {
                    if let InstKind::Copy { dest, value: Operand::Local(source) } = &copied.kind
                        && *dest == held
                        && !sources.contains(source)
                    {
                        sources.push(*source);
                    }
                }
            }
            let insts = || body.iter().filter_map(|at| function.blocks.get(*at)).flat_map(|held| &held.insts);
            let changes_it = insts().any(|later| {
                sources.iter().any(|source| {
                    let held = Operand::Local(*source);
                    match &later.kind {
                        InstKind::SetIndex { object, .. } | InstKind::SetField { object, .. } => {
                            object == source
                        }
                        InstKind::Builtin { args, changes: true, .. } => args.first() == Some(&held),
                        InstKind::Call { args, .. } => args.contains(&held),
                        _ => false,
                    }
                })
            });
            // Every read of the item, anywhere, only looks inside it.
            let only_looked_at = function.blocks.iter().flat_map(|held| &held.insts).all(|other| {
                let mut reads = false;
                visit(&other.kind, |operand| reads |= *operand == Operand::Local(*dest), |_| {});
                !reads || only_looks(&other.kind, *dest)
            }) && function.blocks.iter().all(|held| !reads_in_end(&held.end, *dest));
            if !changes_it && only_looked_at {
                known.borrowed.insert(dest.0);
            }
        }
    }
}

/// Whether a block's way out reads a local.
fn reads_in_end(end: &Terminator, local: LocalId) -> bool {
    match end {
        Terminator::Return(Some(value)) | Terminator::Branch { cond: value, .. } => {
            *value == Operand::Local(local)
        }
        _ => false,
    }
}

/// `mut item = items[key]`, changed, then `items[key] = item`, for a list, a `Table` or a dict: the item
/// is taken out rather than copied, as a `for ref` item is, when it goes back by the same key on
/// every way on and nothing reads the collection meanwhile. The IR reads it into a temporary
/// copied into the name, so both are taken: the copy only hands it over.
fn find_put_back_takes(function: &Function, known: &mut Analysis) {
    for (start, block) in function.blocks.iter().enumerate() {
        for (at, inst) in block.insts.iter().enumerate() {
            let InstKind::GetIndex { dest: read, object: Operand::Local(from), index } = &inst.kind else {
                continue;
            };
            let Some(InstKind::Copy { dest: item, value: Operand::Local(copied) }) =
                block.insts.get(at + 1).map(|next| &next.kind)
            else {
                continue;
            };
            let kept = matches!(
                function.locals.get(from.0).map(|local| &local.ty),
                Some(Type::List(_) | Type::Array(..) | Type::Table(_) | Type::Dict(..))
            );
            if !kept
                || copied != read
                || read == from
                || item == from
                || [*read, *item]
                    .iter()
                    .any(|local| known.writes.get(local.0) != Some(&1) || known.taken.contains(&local.0))
                || known.borrowed.contains(&item.0)
                || known.reads.get(read.0) != Some(&1)
            {
                continue;
            }
            let Some((root, path)) = put_back_to(function, known, *item, index) else { continue };
            if comes_from(function, known, *from, (root, &path))
                && stays_apart(function, known, (start, at + 2), (*item, *from, index), (root, &path))
            {
                for local in [*read, *item] {
                    known.taken.insert(local.0);
                    known.items_taken.insert(local.0, (root, path.clone()));
                }
                known.put_back_reads.insert(item.0, *read);
            }
        }
    }
}

/// Where a local is put back, when that's one place and by a key equal to `index`: the same
/// operand, or the same item read again of something nothing changes.
fn put_back_to(
    function: &Function,
    known: &Analysis,
    item: LocalId,
    index: &Operand,
) -> Option<(LocalId, Vec<String>)> {
    let mut home: Option<(LocalId, &Vec<String>)> = None;
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        if let InstKind::SetIndex { object, path, index: at, value: Operand::Local(put) } = &inst.kind
            && *put == item
        {
            if home.is_some() || !same_key(function, known, at, index) {
                return None;
            }
            home = Some((*object, path));
        }
    }
    home.map(|(root, path)| (root, path.clone()))
}

/// Whether two keys are equal wherever both are read: the same operand, or locals each written
/// once by the same read of an item, of locals nothing writes again.
fn same_key(function: &Function, known: &Analysis, one: &Operand, other: &Operand) -> bool {
    if one == other {
        return true;
    }
    let (Operand::Local(one), Operand::Local(other)) = (one, other) else { return false };
    let once = |local: &LocalId| known.writes.get(local.0) == Some(&1);
    let steady = |operand: &Operand| match operand {
        Operand::Local(local) => known.writes.get(local.0).copied().unwrap_or(0) <= 1,
        Operand::Int(_) | Operand::Str(_) | Operand::Bool(_) => true,
        _ => false,
    };
    let read_by = |local: &LocalId| {
        function.blocks.iter().flat_map(|block| &block.insts).find_map(|inst| match &inst.kind {
            InstKind::GetIndex { dest, object, index } if dest == local => {
                Some((object.clone(), index.clone()))
            }
            _ => None,
        })
    };
    // What a key is read from mustn't change in place either, as `ids.swap(0, 1)`, `ids.reverse()`,
    // `ids[2] = x` or `shuffle(ref ids)` change it: the same read would then find another key.
    let changed_in_place = |operand: &Operand| {
        let Operand::Local(held) = operand else { return false };
        function.blocks.iter().flat_map(|block| &block.insts).any(|inst| match &inst.kind {
            InstKind::SetIndex { object, .. } | InstKind::SetField { object, .. } => object == held,
            InstKind::Builtin { args, changes: true, .. } => args.first() == Some(operand),
            InstKind::Call { args, .. } => args.contains(operand),
            _ => false,
        })
    };
    once(one)
        && once(other)
        && read_by(one).zip(read_by(other)).is_some_and(|((object, index), (object2, index2))| {
            object == object2
                && index == index2
                && steady(&object)
                && steady(&index)
                && !changed_in_place(&object)
                && !changed_in_place(&index)
        })
}

/// `for ref item in table`: the same for a `Table`'s slot, read into the loop's name and put
/// back by the `Id` asked of the same slot just before. Taken, the item has one owner while the
/// body runs, so changing it changes it where it is, and putting it back moves it home.
fn find_slot_takes(function: &Function, known: &mut Analysis) {
    for (start, block) in function.blocks.iter().enumerate() {
        for (at, inst) in block.insts.iter().enumerate() {
            let InstKind::Builtin { dest: Some(dest), name, args, .. } = &inst.kind else { continue };
            let [Operand::Local(from), _] = args.as_slice() else { continue };
            if name != "Table.at_slot"
                || dest == from
                || known.writes.get(dest.0) != Some(&1)
                || known.taken.contains(&dest.0)
                || known.borrowed.contains(&dest.0)
            {
                continue;
            }
            // The slot's `Id`, asked just before at the same slot.
            let id =
                block.insts.get(..at).unwrap_or_default().iter().rev().find_map(|asked| match &asked.kind {
                    InstKind::Builtin { dest: Some(id), name, args: asked_args, .. }
                        if name == "Table.slot_id" && asked_args.get(1) == args.get(1) =>
                    {
                        Some((*id, asked_args.first().cloned()))
                    }
                    _ => None,
                });
            let Some((id, Some(Operand::Local(asked_of)))) = id else { continue };
            let index = Operand::Local(id);
            if known.writes.get(id.0) != Some(&1) {
                continue;
            }
            let Some((root, path)) = written_back_to(function, *dest, &index) else { continue };
            // Both read the same `Table`: the one the item goes back into.
            if comes_from(function, known, *from, (root, &path))
                && comes_from(function, known, asked_of, (root, &path))
                && stays_apart(function, known, (start, at + 1), (*dest, *from, &index), (root, &path))
            {
                known.taken.insert(dest.0);
                known.loop_taken.insert(dest.0);
                known.slot_homes.insert(dest.0, (root, path.clone()));
                // A field's `Table` read for the item is only looked at, and nothing reads it
                // again before the item is home, so holding it would make the slot shared.
                if *from != root {
                    known.borrowed.insert(from.0);
                }
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

/// A loop's own count, as the index a `for` over a list reads each item at: short of the limit
/// wherever the body reads it, as a copy of it is. Only when every index it's used as is in the
/// body, before anything steps it there, since past the loop it has reached the limit; and only
/// into lists read as lists, since a `Table`'s walk, a string and the like check for themselves.
fn find_counts_inside(function: &Function, known: &mut Analysis) {
    for count in function.counts.iter().filter(|count| !count.down) {
        let held = Operand::Local(count.count);
        let header = function.blocks.iter().position(|block| {
            let Terminator::Branch { cond: Operand::Local(cond), .. } = &block.end else { return false };
            block.insts.iter().any(|inst| {
                matches!(&inst.kind, InstKind::Binary { dest, op: BinOp::Lt, lhs, rhs: Operand::Local(stop) }
                    if dest == cond && *lhs == held && *stop == count.stop)
            })
        });
        let Some(header) = header else { continue };
        let Some(Terminator::Branch { then, .. }) = function.blocks.get(header).map(|block| &block.end)
        else {
            continue;
        };
        let body = loop_blocks(function, header, then.0);
        let indexes_in_body = function.blocks.iter().enumerate().all(|(at, block)| {
            let mut stepped = false;
            block.insts.iter().all(|inst| {
                let (indexes, list_read) = match &inst.kind {
                    InstKind::GetIndex { dest, object, index } => (
                        *index == held,
                        matches!(function.type_of(object), Some(Type::List(_)))
                            && !known.walk_items.contains_key(&dest.0),
                    ),
                    InstKind::SetIndex { object, path, index, .. } => (
                        *index == held,
                        path.is_empty()
                            && matches!(function.type_of(&Operand::Local(*object)), Some(Type::List(_))),
                    ),
                    _ => (false, true),
                };
                let fine = !indexes || (list_read && body.contains(&at) && !stepped);
                visit(&inst.kind, |_| {}, |local| stepped |= local == count.count);
                fine
            })
        });
        let places_in_body = function
            .places
            .iter()
            .flat_map(|place| &place.steps)
            .all(|step| !matches!(step, PlaceStep::Item(index) if *index == held));
        if indexes_in_body && places_in_body {
            known.below.insert(count.count.0, count.stop);
        }
    }
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
        let Terminator::Jump(header_at) = &block.end else { continue };
        let Some(header) = function.blocks.get(header_at.0) else { continue };
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
        // Only a list made here, which has one owner. A constant list is one block every reader
        // shares and nothing frees, so appending to it with no count check would write into it.
        let made_here = block
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::MakeList { dest, items } if dest.0 == list && items.is_empty()));
        if made_here {
            let zeros_counted = fills_with_zeros(function, block, (header, body), list, known);
            let zeros = zeros_counted.is_some();
            match appends_within_room(function, (block, header_at.0), then.0, list) {
                Some(Within::Room) => {
                    known.room_enough.insert(list);
                }
                // A list made whole of zeros has no appends to count, and keeps its length.
                Some(Within::RoomLeavingTo(after)) => {
                    known.room_enough.insert(list);
                    if !zeros {
                        known.length_kept.insert(list, after);
                    }
                }
                None => {}
            }
            known.reservations.insert(at, (list, limit));
            if let Some(count) = zeros_counted {
                known.zero_filled.insert(list);
                known.zero_counts.insert(list, count);
            }
        }
    }
}

/// What `appends_within_room` found: the appends stay within the room, and whether the loop
/// leaves only by its header, to the block after it.
enum Within {
    Room,
    RoomLeavingTo(usize),
}

/// Whether the loop after `before`, at `header`, appends to `list` at most as many times as
/// the room reserved for it: it counts up by 1 from a constant of 0 or more, set just before,
/// to a limit nothing in the loop changes, so it turns at most `limit` times; its body, entered
/// only from the header, appends once; and nothing else in the loop mentions the list, so it
/// isn't grown, shared or changed any other way while it's filled.
fn appends_within_room(
    function: &Function,
    (before, header_at): (&Block, usize),
    body_at: usize,
    list: usize,
) -> Option<Within> {
    let header = function.blocks.get(header_at)?;
    let Terminator::Branch { cond: Operand::Local(cond), otherwise, .. } = &header.end else { return None };
    let compare = header.insts.iter().find_map(|inst| match &inst.kind {
        InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(count), rhs } if dest == cond => {
            Some((*count, rhs.clone()))
        }
        _ => None,
    });
    let (count, limit) = compare?;
    if !function.counts.iter().any(|found| found.count == count && !found.down) {
        return None;
    }
    let start = before.insts.iter().rev().find_map(|inst| match &inst.kind {
        InstKind::Copy { dest, value } if *dest == count => Some(value.clone()),
        _ => None,
    });
    if !matches!(start, Some(Operand::Int(start)) if start >= 0) {
        return None;
    }
    let ways_in = function
        .blocks
        .iter()
        .filter(|block| block.end.targets().iter().any(|next| next.0 == body_at))
        .count();
    if ways_in != 1 {
        return None;
    }
    // The loop's own blocks: those its body reaches without going through the header again,
    // that lead back to it. A loop around this one is left out: it isn't this loop's turn.
    let reached = |from: usize, forward: bool| -> HashSet<usize> {
        let mut seen = HashSet::from([from]);
        let mut waiting = vec![from];
        while let Some(at) = waiting.pop() {
            if forward && at == header_at {
                continue;
            }
            let next: Vec<usize> = if forward {
                function
                    .blocks
                    .get(at)
                    .map(|block| block.end.targets().iter().map(|target| target.0).collect())
                    .unwrap_or_default()
            } else {
                function
                    .blocks
                    .iter()
                    .enumerate()
                    .filter(|(_, block)| block.end.targets().iter().any(|target| target.0 == at))
                    .map(|(from, _)| from)
                    .collect()
            };
            for block in next {
                if seen.insert(block) {
                    waiting.push(block);
                }
            }
        }
        seen
    };
    let (ahead, behind) = (reached(body_at, true), reached(header_at, false));
    let looped: Vec<usize> = ahead.intersection(&behind).copied().chain([header_at]).collect();
    let mut appends = 0;
    for at in &looped {
        let block = function.blocks.get(*at)?;
        for inst in &block.insts {
            let (mut read, mut written) = (false, false);
            visit(
                &inst.kind,
                |operand| read |= *operand == Operand::Local(LocalId(list)),
                |local| {
                    written |= local.0 == list;
                },
            );
            let touched = read || written;
            if let Operand::Local(stop) = &limit
                && inst.kind.dest() == Some(*stop)
            {
                return None;
            }
            if !touched {
                continue;
            }
            let appended = matches!(&inst.kind, InstKind::Builtin { name, args, .. }
                if name == "list.append" && args.first() == Some(&Operand::Local(LocalId(list))) && args.len() == 2);
            if !appended || *at != body_at {
                return None;
            }
            appends += 1;
        }
    }
    if appends != 1 {
        return None;
    }
    // Whether it leaves only by the header, to a block nothing else comes to: then the length
    // can be kept aside while it fills, and written there.
    let only_header = looped.iter().filter(|at| **at != header_at).all(|at| {
        function.blocks.get(*at).is_some_and(|block| {
            matches!(block.end, Terminator::Jump(_) | Terminator::Branch { .. })
                && block.end.targets().iter().all(|next| looped.contains(&next.0))
        })
    });
    let into_after = function.blocks.iter().filter(|block| block.end.targets().contains(otherwise)).count();
    Some(if only_header && into_after == 1 { Within::RoomLeavingTo(otherwise.0) } else { Within::Room })
}

/// Whether a loop does nothing but append a zero to `list` each turn, counting up from 0 by 1,
/// so that it appends exactly as many as its limit says: its count, if it does.
fn fills_with_zeros(
    function: &Function,
    before: &Block,
    (header, body): (&Block, &Block),
    list: usize,
    known: &Analysis,
) -> Option<LocalId> {
    let Terminator::Branch { cond: Operand::Local(cond), .. } = &header.end else { return None };
    let count = header.insts.iter().find_map(|inst| match &inst.kind {
        InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(count), .. } if dest == cond => {
            Some(*count)
        }
        _ => None,
    });
    let count = count?;
    if !function.counts.iter().any(|found| found.count == count && !found.down) {
        return None;
    }
    let starts_at_zero = before.insts.iter().rev().find_map(|inst| match &inst.kind {
        InstKind::Copy { dest, value } if *dest == count => Some(*value == Operand::Int(0)),
        _ => None,
    });
    if starts_at_zero != Some(true) || header.insts.len() != 1 {
        return None;
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
            _ => return None,
        }
    }
    // The turn ends by stepping the count and nothing else.
    let Terminator::Jump(latch) = body.end else { return None };
    (appends == 1 && function.blocks.get(latch.0).is_some_and(|latch| latch.insts.len() == 1))
        .then_some(count)
}

impl Analysis {
    /// Nothing known yet, about a function with this many locals.
    fn empty(count: usize) -> Analysis {
        Analysis {
            safe: HashSet::new(),
            nonzero: HashSet::new(),
            below: HashMap::new(),
            length_of: HashMap::new(),
            reads: vec![0; count],
            writes: vec![0; count],
            shared: HashSet::new(),
            changed: HashSet::new(),
            maybe_marked: HashSet::new(),
            given_mut: HashSet::new(),
            read_again: HashSet::new(),
            borrowed: HashSet::new(),
            borrowed_from: HashMap::new(),
            taken: HashSet::new(),
            items_taken: HashMap::new(),
            slot_homes: HashMap::new(),
            put_back_reads: HashMap::new(),
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
            zero_counts: HashMap::new(),
            room_enough: HashSet::new(),
            length_kept: HashMap::new(),
            last_copies: HashSet::new(),
            last_reads: HashSet::new(),
            last_made: HashSet::new(),
            enumerated: HashMap::new(),
            map_walks: HashMap::new(),
            walk_keys: HashSet::new(),
            walk_items: HashMap::new(),
            pairs: HashMap::new(),
            guarded_reads: HashSet::new(),
            param_ranges: Vec::new(),
        }
    }

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

    /// Whether the read of an item at this block and place in it can't be out of range, by what
    /// the guard before it says.
    pub(crate) fn guarded_read(&self, at: (usize, usize)) -> bool {
        self.guarded_reads.contains(&at)
    }

    /// Whether `list[index]` can't be out of range. A copy standing for the list a loop goes
    /// through has its length, since the loop changes only the item it's on.
    pub(crate) fn inside(&self, list: LocalId, index: &Operand) -> bool {
        let Operand::Local(index) = index else { return false };
        let Some(limit) = self.below.get(&index.0) else { return false };
        let Some(&measured) = self.length_of.get(&limit.0) else { return false };
        measured == list.0 || self.aliases.get(&measured) == Some(&list)
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

    /// Whether reading a result's value or error at this place is the last anything does with
    /// the result.
    pub(crate) fn is_last_read(&self, block: usize, index: usize) -> bool {
        self.last_reads.contains(&(block, index))
    }

    /// Whether a struct made at this place in this block is the last anything does with this
    /// local it's made with.
    pub(crate) fn is_last_made(&self, (block, index): (usize, usize), local: LocalId) -> bool {
        self.last_made.contains(&(block, index, local.0))
    }

    /// Whether a local is written once and read once.
    pub(crate) fn used_once(&self, local: LocalId) -> bool {
        self.reads.get(local.0) == Some(&1) && self.writes.get(local.0) == Some(&1)
    }

    /// Whether this function changes a list in a local nothing else here comes to hold, so
    /// it's enough to see that it has one owner when the local gets it.
    pub(crate) fn alone_once_checked(&self, local: LocalId) -> bool {
        self.changed.contains(&local.0)
            && !self.shared.contains(&local.0)
            && !self.maybe_marked.contains(&local.0)
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

    /// The local an item copied out with `mut` was first read into, which holds what was taken.
    pub(crate) fn put_back_read(&self, item: LocalId) -> Option<LocalId> {
        self.put_back_reads.get(&item.0).copied()
    }

    /// Where the `Table` is that a `for ref` item was taken out of.
    pub(crate) fn slot_home(&self, local: LocalId) -> Option<&(LocalId, Vec<String>)> {
        self.slot_homes.get(&local.0)
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
    // Where the walk is asked for: each block that does, with the place after it there.
    let mut after = Vec::new();
    let mut before: HashMap<usize, Vec<usize>> = HashMap::new();
    for (at, block) in function.blocks.iter().enumerate() {
        for next in block.end.targets() {
            before.entry(next.0).or_default().push(at);
        }
        if let Some(index) = block.insts.iter().position(
            |inst| matches!(&inst.kind, InstKind::Builtin { dest: Some(dest), .. } if *dest == made),
        ) {
            after.push((at, index + 1));
        }
    }
    // Asked for in more than one block, it isn't known which the walk starts from.
    let [(start, first)] = after[..] else { return false };
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
        // What comes before the walk's asked for in its own block always runs before it's asked
        // for again, which starts the walk over, so only a read in that block can see it.
        let skipped = if *at == start && (read_at != start || !to_read_comes_back(&to_read, &before, start)) {
            first
        } else {
            0
        };
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
        let of_text = function.locals.get(list.0).is_some_and(|local| local.ty == Type::Str);
        let walks = matches!(
            name.as_str(),
            "Table.items" | "Table.values" | "dict.items" | "dict.values" | "dict.keys"
        );
        // A loop over a map can't change it, as the checker sees to, so a walk only needs nothing
        // to change it from when this was asked for until its loop's done. Something else can
        // reach a map through a `shared` handle, so unless it's known steady, the local holds an
        // owner of its own for the walk, which makes such a change copy it.
        let holds_it = !known.is_borrowed(*list) && !known.is_taken(*list) && known.alias(*list).is_none();
        let steady = steady || (walks && holds_it);
        if !(name == "enumerate" || walks) || of_text || !steady || known.writes.get(made.0) != Some(&1) {
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
        for (pair, _, at, index) in
            pairs.iter().filter(|_| !matches!(name.as_str(), "Table.values" | "dict.values" | "dict.keys"))
        {
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
                known
                    .walk_items
                    .insert(item.0, (*list, matches!(name.as_str(), "Table.items" | "dict.items")));
                if name == "dict.keys" {
                    known.walk_keys.insert(item.0);
                }
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
            if let InstKind::GetOk { value: Operand::Local(from), .. }
            | InstKind::GetErr { value: Operand::Local(from), .. } = &inst.kind
                && !live.contains(&from.0)
                && !leaned_on.contains(&from.0)
                && from.0 >= function.params
            {
                known.last_reads.insert((at, index));
            }
            if let InstKind::Make { fields, .. } = &inst.kind {
                for field in fields {
                    if let Operand::Local(from) = field
                        && fields.iter().filter(|other| *other == field).count() == 1
                        && !live.contains(&from.0)
                        && !leaned_on.contains(&from.0)
                        && from.0 >= function.params
                    {
                        known.last_made.insert((at, index, from.0));
                    }
                }
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
pub(crate) fn is_socket_method(name: &str) -> bool {
    // A `File` lent by `fs.create` or `fs.open` is a handle into the runtime, as a socket is, and
    // so are SQLite's `Database` and `Transaction`.
    name.starts_with("Connection.")
        || name.starts_with("Server.")
        || name.starts_with("UdpSocket.")
        || name.starts_with("File.")
        || name.starts_with("Database.")
        || name.starts_with("Transaction.")
}

/// Whether a builtin can fail where it runs: in the runtime, or by a check of its own.
pub(crate) fn builtin_fails(function: &Function, name: &str, args: &[Operand]) -> bool {
    // A capability's method runs in the runtime, which can fail it, as when it's narrowed.
    let capability = args
        .first()
        .and_then(|first| function.type_of(first))
        .is_some_and(|ty| matches!(&ty, Type::Named(name) if fennec_layout::Cap::named(name).is_some()));
    // A connection's or server's method runs in the runtime too.
    capability
        // `u8(x)` stops the program when `x` doesn't fit.
        || fennec_check::Num::named(name).is_some_and(|num| !num.is_float())
        // A stop can cut a channel's wait short.
        || matches!(
            name,
            "intrinsics.channel_new" | "intrinsics.channel_new_every" | "intrinsics.channel_send" | "intrinsics.channel_take"
        )
        // A launch fails with no thread to run on, and a wait for a task can be cut short.
        || matches!(name, "intrinsics.launch" | "intrinsics.launch_take")
        || is_socket_method(name)
        // Sorting what isn't a plain number, string or bool orders by its type's own `<`, which
        // can fail.
        || (name == "sorted"
            && args.first().and_then(|list| function.type_of(list)).is_some_and(|ty| {
                !matches!(&ty, Type::List(item) if matches!(**item, Type::Int | Type::Float | Type::Str | Type::Bool))
            }))
        || name.starts_with("crypto.")
        || name.starts_with("compress.")
        || name.starts_with("unicode.")
        || name.starts_with("json.")
        || name.starts_with("toml.")
        || matches!(
            name,
            "math.sqrt"
                    | "Buffer.pop"
                    | "Buffer.insert"
                    | "Buffer.remove"
                    | "json.dumps"
                    | "intrinsics.text_part"
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
                    | "Table.remove_where"
                    | "Table.modify"
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
                    | "main.result"
                    | "bytes"
                    | "set.remove"
                    | "Regex.replace"
                    | "Regex.split"
                    | "Regex.find_all"
                    | "Regex.search"
                    | "Regex.match"
                    | "Regex.fullmatch"
                    // A range's body can fail on any thread, and the call passes it up.
                    | "parallel.for"
                    | "parallel.serve"
                    | "parallel.peer"
                    | "parallel.label"
                    | "parallel.map"
                    | "parallel.stream_next"
                    | "parallel.sum"
        )
}

fn fails_itself(function: &Function, known: &Analysis) -> bool {
    // What's worked out for a local of a sized type is checked to fit it.
    let sized =
        |dest: &LocalId| matches!(function.locals.get(dest.0).map(|local| &local.ty), Some(Type::Sized(_)));
    let inst_fails = |inst: &InstKind| match inst {
        InstKind::Binary { dest, op, .. }
            if sized(dest) && !matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) =>
        {
            true
        }
        InstKind::Unary { dest, op: UnaryOp::Neg, .. } if sized(dest) => true,
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
        InstKind::Builtin { name, args, .. } => builtin_fails(function, name, args),
        InstKind::GetIndex { object: Operand::Local(list), index, .. } => !known.inside(*list, index),
        InstKind::SetIndex { object, path, index, .. } => !path.is_empty() || !known.inside(*object, index),
        InstKind::GetIndex { .. }
        | InstKind::Call { .. }
        | InstKind::Format { .. }
        | InstKind::Slice { .. }
        | InstKind::CallClosure { .. }
        | InstKind::Resume { .. }
        | InstKind::Lock { .. }
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
        #[expect(
            clippy::iter_over_hash_type,
            reason = "each round only turns parameters on, so it settles the same in any order"
        )]
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

/// How many rounds `param_ranges` narrows what's known of parameters through: a helper's
/// helper's helper is known after three.
const PARAM_ROUNDS: usize = 4;

/// What every call gives each function's int parameters. A function something other than a call
/// by name may call, like `main`, a closure, a generator or one nothing calls, may be given
/// anything. Every round starts from what the last found, knowing nothing at first, and finds
/// each call's arguments with its function's parameters as that says. So each round's ranges
/// hold of every call wherever it stops; rounds go on only while they say more.
fn param_ranges<'p>(
    program: &'p Program,
    changes: &dyn Fn(&str) -> Vec<usize>,
    keeps: &dyn Fn(&str, usize) -> bool,
) -> HashMap<&'p str, Vec<Range>> {
    let every = || program.functions.iter().chain(&program.constants);
    let params: HashMap<&str, usize> =
        every().map(|function| (function.name.as_str(), function.params)).collect();
    let other_ways = called_other_ways(program);
    let mut known: HashMap<&str, Vec<Range>> = HashMap::new();
    let mut calls: HashMap<&str, Vec<(&str, Vec<Range>)>> = HashMap::new();
    let mut changed: HashSet<&str> = every().map(|function| function.name.as_str()).collect();
    for _ in 0..PARAM_ROUNDS {
        for function in every().filter(|function| changed.contains(function.name.as_str())) {
            let given = known.get(function.name.as_str()).map_or(&[][..], Vec::as_slice);
            calls.insert(function.name.as_str(), call_ranges(function, (changes, keeps), given));
        }
        let mut next: HashMap<&str, Vec<Range>> = HashMap::new();
        let mut anything: HashSet<&str> = HashSet::new();
        for function in every() {
            for (callee, args) in calls.get(function.name.as_str()).into_iter().flatten() {
                if params.get(callee) != Some(&args.len()) {
                    anything.insert(callee);
                    continue;
                }
                match next.get_mut(callee) {
                    Some(held) => {
                        for (range, arg) in held.iter_mut().zip(args) {
                            *range = (range.0.min(arg.0), range.1.max(arg.1));
                        }
                    }
                    None => {
                        next.insert(callee, args.clone());
                    }
                }
            }
        }
        next.retain(|name, ranges| {
            !other_ways.contains(name)
                && !anything.contains(name)
                && ranges.iter().any(|range| *range != FULL)
        });
        changed = every()
            .map(|function| function.name.as_str())
            .filter(|name| next.get(name) != known.get(name))
            .collect();
        known = next;
        if changed.is_empty() {
            break;
        }
    }
    known
}

/// The functions something other than a direct call may call, so whose parameters may be
/// anything: `main`, a closure's or a generator's, a constant's, and one no call in the program
/// names, like a test.
fn called_other_ways(program: &Program) -> HashSet<&str> {
    let every = || program.functions.iter().chain(&program.constants);
    let mut other_ways: HashSet<&str> = HashSet::from(["main"]);
    let mut called = HashSet::new();
    for inst in every().flat_map(|function| &function.blocks).flat_map(|block| &block.insts) {
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
    for function in every().filter(|function| !called.contains(function.name.as_str())) {
        other_ways.insert(function.name.as_str());
    }
    other_ways
}

/// What each direct call in a function gives its callee, by argument, with the function's own
/// parameters in the ranges given.
fn call_ranges<'p>(
    function: &'p Function,
    (changes, keeps): (Changes<'_>, Keeps<'_>),
    params: &[Range],
) -> Vec<(&'p str, Vec<Range>)> {
    let has_calls = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .any(|inst| matches!(&inst.kind, InstKind::Call { args, .. } if !args.is_empty()));
    if !has_calls {
        return Vec::new();
    }
    let mut known = Analysis::empty(function.locals.len());
    known.param_ranges = given_ranges(function, params);
    count_uses(function, (changes, keeps), &mut known);
    let stepped: HashSet<usize> = function.counts.iter().map(|count| count.count.0).collect();
    let (facts, multi) = ranges(function, changes, &mut known, &stepped);
    let mut found = Vec::new();
    for (at, block) in function.blocks.iter().enumerate() {
        for inst in &block.insts {
            if let InstKind::Call { function: callee, args, .. } = &inst.kind {
                let given = args.iter().map(|arg| range_of(&facts, &multi, arg, at)).collect();
                found.push((callee.as_str(), given));
            }
        }
    }
    found
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
    let params = param_ranges(program, &changes, &keeps);
    let analyses: HashMap<&str, Analysis> = every()
        .map(|function| {
            let given = params.get(function.name.as_str()).map_or(&[][..], Vec::as_slice);
            (function.name.as_str(), analyze(function, &changes, &keeps, given))
        })
        .collect();
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
    let crate::uniqueness::Needs { always: needs, may: may_need } = needed(program, &functions, &analyses);
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
    let tasks = crate::ticks::tasks(&functions);
    Whole {
        functions,
        analyses,
        can_fail,
        generators,
        settles,
        lengths,
        builders,
        non_negative: crate::signs::non_negative(program),
        needs,
        may_need,
        constants,
        on_workers,
        cells: crate::parallel::has_cells(program),
        tasks,
        runtime_closures: crate::closures::runtime_holds_closures(program),
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    const TYPES: &str = "struct Unit:\n    tags: list[int]\n    hp: int\n\nstruct World:\n    units: list[Unit]\n    log: list[int]\n\nerror Bad:\n    why: str\n\ndef total(units: list[Unit]) -> int:\n    return len(units)\n\ndef size(world: World) -> int:\n    return len(world.log)\n\n";

    /// Whether the `unit` of a function called `go` is taken out for its loop's whole body.
    fn taken(body: &str) -> Result<bool, String> {
        let source = format!("{TYPES}{body}");
        let module = fennec_syntax::parse(&source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[]);
        let at = function.locals.iter().position(|local| local.name.as_deref() == Some("unit"));
        Ok(known.taken_over_loop(LocalId(at.ok_or("no `unit`")?)))
    }

    #[test]
    fn a_for_mut_item_is_taken_on_every_way_out_of_its_body() -> Result<(), String> {
        for body in [
            "def go(ref units: list[Unit]):\n    for ref unit in units:\n        unit.tags.append(1)\n",
            "def go(ref units: list[Unit]):\n    for ref unit in units:\n        if unit.hp == 0:\n            continue\n        unit.tags.append(1)\n        if unit.hp == 1:\n            break\n        unit.hp = 2\n",
            "def go(ref units: list[Unit]) -> int:\n    for ref unit in units:\n        unit.tags.append(1)\n        if unit.hp == 1:\n            return len(unit.tags)\n    return 0\n",
            "def go(ref units: list[Unit]) -> int fails Bad:\n    for ref unit in units:\n        unit.tags.append(1)\n        if unit.hp == 1:\n            fail Bad(\"no\")\n    return 0\n",
            "def go(ref world: World):\n    for ref unit in world.units:\n        unit.tags.append(1)\n",
            "def go(ref world: World):\n    for ref unit in world.units:\n        world.log.append(unit.hp)\n        unit.tags.append(1)\n",
        ] {
            assert!(taken(body)?, "not taken: {body}");
        }
        Ok(())
    }

    /// Whether the one list a function called `go` fills is filled with no room check: the
    /// list its comprehension makes, before it's given a name.
    fn within_room(body: &str) -> Result<bool, String> {
        let module = fennec_syntax::parse(body).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[]);
        Ok(!known.room_enough.is_empty())
    }

    /// A comprehension over a count from 0 or more fills its list with no room check: room
    /// for every turn is made first, and nothing else touches the list while it's filled. One
    /// counting from below 0 or from a variable could outgrow its room, and one with a filter
    /// appends somewhere other than its body's first block, so those keep the check.
    #[test]
    fn only_a_list_filled_within_its_room_skips_the_check() -> Result<(), String> {
        let filled = |body: &str| format!("def go(n: int) -> int:\n{body}    return len(xs)\n");
        for body in ["    xs = [i * 2 for i in range(n)]\n", "    xs = [i for i in range(1, n)]\n"] {
            assert!(within_room(&filled(body))?, "kept the check: {body}");
        }
        for body in [
            "    xs = [i for i in range(-3, n)]\n",
            "    start = n - 5\n    xs = [i for i in range(start, n)]\n",
            "    xs = [i for i in range(n) if i % 2 == 0]\n",
        ] {
            assert!(!within_room(&filled(body))?, "skipped the check: {body}");
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
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[]);
        Ok((0..function.locals.len()).filter(|at| known.is_taken(LocalId(*at))).count())
    }

    /// Only `+=` on a text in a field takes it out: added to in any other way, or for a number,
    /// what's in the field has to last until the new value is put there.
    #[test]
    fn a_text_growing_in_a_field_is_taken_out_of_it() -> Result<(), String> {
        let grows = "def go(ref note: Note, more: str):\n    note.text += more\n";
        assert_eq!(taken_locals(grows)?, 1);
        let makes = "def go(ref note: Note, more: str):\n    note.text = note.text + more\n";
        assert_eq!(taken_locals(makes)?, 0);
        let number = "def go(ref note: Note):\n    note.hp += 1\n";
        assert_eq!(taken_locals(number)?, 0);
        Ok(())
    }

    /// The list's place is out of date while the body runs, so a body that could read it isn't
    /// taken: reading a stale address gives a wrong answer and no failure.
    #[test]
    fn a_for_mut_item_is_not_taken_when_the_body_can_read_its_list() -> Result<(), String> {
        for body in [
            "def go(ref units: list[Unit]):\n    for ref unit in units:\n        unit.tags.append(units[0].hp)\n",
            "def go(ref units: list[Unit]):\n    for ref unit in units:\n        unit.hp += len(units)\n",
            "def go(ref units: list[Unit]):\n    for ref unit in units:\n        unit.hp += total(units)\n",
            "def go(ref world: World):\n    for ref unit in world.units:\n        unit.hp += len(world.units)\n",
            "def go(ref world: World):\n    for ref unit in world.units:\n        unit.hp += size(world)\n",
            "def go(units: list[Unit]) -> int:\n    mut found = 0\n    for unit in units:\n        found += unit.hp\n    return found\n",
        ] {
            assert!(!taken(body)?, "taken: {body}");
        }
        Ok(())
    }
}
