//! What's known about each function before emitting it: which of its checks can't fail, from
//! `fennec_ir::facts`, which lists keep one length, who a value can move to, and so which
//! functions can fail at all.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::keys::Key;
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

/// Whether a dict of this type holds its values in its entries, as the program's layouts say.
type InEntries<'f> = &'f dyn Fn(&Type) -> bool;

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
    /// Those of them taken out only when the append comes, as its argument is worked out between
    /// the read and it: by the block and instruction the append is, the field's local, the item
    /// it's read from and the field.
    path_waits: HashMap<(usize, usize), (LocalId, LocalId, String)>,
    /// The fields' locals of [`Self::path_waits`].
    path_waiting: HashSet<usize>,
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
    /// A slice of a string read only by one `split` or `find`, which reads it where it is. The
    /// string and where it starts and ends. It's never made: a string in a block has the call
    /// read that range of it (`fennec_str_split_range`, `fennec_str_find_range`), and an inline
    /// one is sliced natively, as before.
    pub(crate) slice_ranges: HashMap<usize, SliceKey>,
    /// The slice and its copy, which are never made.
    pub(crate) unmade: HashSet<usize>,
    /// Locals standing for a list that a `for` loop goes through while changing only the item
    /// it's on: a copy of the list would say the same, and cost a whole list.
    aliases: HashMap<usize, LocalId>,
    /// Loops that change a list or a `Table` nothing in them can give another owner, which is
    /// made alone once on the way in rather than at every change (see [`find_alone_in_loops`]).
    pub(crate) alone_in_loops: Vec<AloneInLoop>,
    /// List locals whose length nothing in the function changes: their length is read where
    /// they're given their value and kept, so checks against it can leave a loop (see
    /// `steady_lengths`).
    pub(crate) steady_lengths: HashSet<usize>,
    /// Reads of a list's item at `i` followed in the same block by one at `i + 1`, by the read's
    /// place: `i`, and the read's own index, `i` or `i + 1` (see `find_adjacent_reads`).
    pub(crate) adjacent_reads: HashMap<(usize, usize), (LocalId, LocalId)>,
    /// A block ending in a jump to a loop that fills lists made in that block: each list, how
    /// many items the loop adds to it at most, and the empty list it was copied from, made just
    /// before in the same block, when it was.
    pub(crate) reservations: HashMap<usize, Vec<Reserved>>,
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
    /// Reads of a list's item, by block and place in it, whose index counts from one item of
    /// another list up to before another of its items: in range wherever every item of that list
    /// is from 0 to the read list's length, which the function checks once on its way in. Each
    /// is the check, by its place in `range_scans`. See `find_ranged_reads`.
    pub(crate) ranged_reads: HashMap<(usize, usize), usize>,
    /// The checks of item ranges those reads lean on, each made at the end of the block going into
    /// the outermost loop the reads are in.
    pub(crate) range_scans: Vec<RangeScan>,
    /// The loops written twice for those reads: once checking them as today, and once, chosen on
    /// the way in when the check held, reading with no check (`find_versioned_loops`).
    pub(crate) versioned: Vec<Versioned>,
    /// The int parameters every call gives a known range, with it (see `param_ranges`). A build
    /// that checks proofs checks each on the way in.
    pub(crate) param_ranges: Vec<(usize, Range)>,
}

impl Analysis {
    /// Hashes everything this says, the same whenever it says the same: sets and maps in order.
    /// Every field is named, so one added later has to be hashed too.
    pub(crate) fn stable(&self, state: &mut crate::keys::Stable) {
        use crate::keys::{hash_map, hash_set};
        use std::hash::Hash as _;
        let Analysis {
            safe,
            nonzero,
            below,
            length_of,
            reads,
            writes,
            shared,
            changed,
            maybe_marked,
            given_mut,
            read_again,
            borrowed,
            borrowed_from,
            taken,
            items_taken,
            put_back_reads,
            slot_homes,
            path_taken,
            path_waits,
            path_waiting,
            loop_taken,
            places_taken,
            place_parts,
            place_paths,
            places_waiting,
            field_of,
            slice_keys,
            slice_ranges,
            unmade,
            aliases,
            alone_in_loops,
            steady_lengths,
            adjacent_reads,
            reservations,
            zero_filled,
            zero_counts,
            room_enough,
            length_kept,
            last_copies,
            last_reads,
            last_made,
            enumerated,
            map_walks,
            walk_items,
            walk_keys,
            pairs,
            guarded_reads,
            ranged_reads,
            range_scans,
            versioned,
            param_ranges,
        } = self;
        hash_set(safe, state);
        hash_set(nonzero, state);
        hash_map(below, state);
        hash_map(length_of, state);
        reads.hash(state);
        writes.hash(state);
        hash_set(shared, state);
        hash_set(changed, state);
        hash_set(maybe_marked, state);
        hash_set(given_mut, state);
        hash_set(read_again, state);
        hash_set(borrowed, state);
        hash_map(borrowed_from, state);
        hash_set(taken, state);
        hash_map(items_taken, state);
        hash_map(put_back_reads, state);
        hash_map(slot_homes, state);
        hash_map(path_taken, state);
        hash_map(path_waits, state);
        hash_set(path_waiting, state);
        hash_set(loop_taken, state);
        hash_map(places_taken, state);
        hash_map(place_parts, state);
        hash_map(place_paths, state);
        hash_set(places_waiting, state);
        hash_map(field_of, state);
        hash_map(slice_keys, state);
        hash_map(slice_ranges, state);
        hash_set(unmade, state);
        hash_map(aliases, state);
        alone_in_loops.hash(state);
        hash_set(steady_lengths, state);
        hash_map(adjacent_reads, state);
        hash_map(reservations, state);
        hash_set(zero_filled, state);
        hash_map(zero_counts, state);
        hash_set(room_enough, state);
        hash_map(length_kept, state);
        hash_set(last_copies, state);
        hash_set(last_reads, state);
        hash_set(last_made, state);
        hash_map(enumerated, state);
        hash_map(map_walks, state);
        hash_map(walk_items, state);
        hash_set(walk_keys, state);
        hash_map(pairs, state);
        hash_set(guarded_reads, state);
        hash_map(ranged_reads, state);
        range_scans.hash(state);
        versioned.hash(state);
        param_ranges.hash(state);
    }
}

/// What's known about every function, and which can fail.
#[expect(clippy::struct_excessive_bools, reason = "each is a fact of its own about the whole program")]
pub(crate) struct Whole<'p> {
    pub(crate) functions: HashMap<&'p str, &'p Function>,
    pub(crate) analyses: HashMap<&'p str, std::sync::Arc<Analysis>>,
    /// Each function's IR key, where this build worked them out to keep its analyses (see
    /// `kept`), so that its units use them rather than working each out again. Empty otherwise.
    pub(crate) ir_keys: HashMap<&'p str, Key>,
    /// Each analysis's key ([`analysis_key`]), where this build keeps its analyses: a unit's read
    /// of one is a lookup. Empty otherwise.
    pub(crate) analysis_keys: HashMap<&'p str, Key>,
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
    /// The functions that may be handed, to change, part of what a `lock ... as ref` holds.
    pub(crate) under_locks: HashSet<&'p str>,
    /// The functions that give a constant's value, which keep it after the first time.
    pub(crate) constants: HashSet<&'p str>,
    /// The functions that can run on a thread `parallel` uses, which everything else can't.
    pub(crate) on_workers: HashSet<&'p str>,
    /// Whether the program has any `shared` type.
    pub(crate) cells: bool,
    /// The functions that can run as a parked task, which count their loops' turns.
    pub(crate) tasks: HashSet<&'p str>,
    /// The tasks that can call themselves again, through others or not, which count their calls
    /// too (see `ticks::recurring`).
    pub(crate) recurring: HashSet<&'p str>,
    /// Whether its closures are blocks in the runtime's table, since it keeps one where the
    /// runtime reads (see `closures`).
    pub(crate) runtime_closures: bool,
    /// Whether the program has tests built into it, as `fennec test`'s and `fennec run`'s do: a
    /// test's capabilities are fakes, which only the runtime's export for every module asks.
    pub(crate) tested: bool,
    /// Whether any of its generators closes something when it's let go partway, at a `yield`
    /// inside a `with` source's block: only then does letting go of a value ever run program code,
    /// which can fail. Without one, every free is plain.
    pub(crate) abandons: bool,
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
    in_entries: InEntries<'_>,
) -> Analysis {
    let mut known = Analysis::empty(function.locals.len());
    known.param_ranges = given_ranges(function, params);
    let grown = count_uses(function, (changes, keeps), &mut known);
    find_maybe_marked(function, &mut known);
    find_reads_again(function, &mut known);
    find_place_takes(function, changes, &mut known);
    find_borrows(function, changes, &mut known);
    find_live_borrows(function, changes, &mut known);
    find_path_takes(function, changes, &mut known);
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
    known.steady_lengths = steady_lengths(function, &grown, &known);
    find_guarded_reads(function, (&facts, &multi), &mut known);
    find_ranged_reads(function, &mut known);
    known.below.retain(|local, _| facts.range_in(LocalId(*local), 0).is_some_and(|range| range.0 >= 0));
    find_reservations(function, &stepped, &mut known);
    find_slice_keys(function, in_entries, &mut known);
    find_slice_ranges(function, &mut known);
    find_unmade_enumerates(function, &mut known);
    find_counts_inside(function, &mut known);
    find_adjacent_reads(function, &mut known);
    find_alone_in_loops(function, &mut known);
    known
}

/// The list parameters whose length nothing in the function can change, so it can be read once,
/// at the start, and kept. Only parameters: a list made here has its length written here, which
/// LLVM sees and carries to every check itself; a kept copy of it only got in the way, and
/// fannkuch, whose lists are all made in its function, ran 6% more instructions. A parameter's
/// length is only ever in memory, behind calls LLVM can't see past. Left out is a list that's
/// grown or shrunk here (`grown`: a builtin that changes it, or a call that changes what it's
/// given), given another value, handed to a closure, which may change what it's given, standing
/// for or stood for by another local, or taken out of its place. A list made alone again before
/// a change (`fennec_list_unshare`) keeps its length, so it stays. A build that checks proofs
/// checks each use of a kept length against the list's own.
fn steady_lengths(function: &Function, grown: &HashSet<usize>, known: &Analysis) -> HashSet<usize> {
    // Only a list read in a loop gains from it: elsewhere its length is read once anyway.
    let looped = blocks_in_loops(function);
    let mut read_in_loops = HashSet::new();
    for (at, block) in function.blocks.iter().enumerate() {
        if looped.contains(&at) {
            for inst in &block.insts {
                visit(
                    &inst.kind,
                    |operand| {
                        if let Operand::Local(local) = operand {
                            read_in_loops.insert(local.0);
                        }
                    },
                    |_| {},
                );
            }
        }
    }
    let handed: HashSet<usize> = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.kind {
            InstKind::CallClosure { args, .. } => Some(args),
            _ => None,
        })
        .flatten()
        .filter_map(|arg| match arg {
            Operand::Local(local) => Some(local.0),
            _ => None,
        })
        .collect();
    let named_twice: HashSet<usize> = known.aliases.iter().flat_map(|(alias, of)| [*alias, of.0]).collect();
    (0..function.locals.len())
        .filter(|at| {
            matches!(function.locals.get(*at).map(|local| &local.ty), Some(Type::List(_)))
                && !grown.contains(at)
                && *at < function.params
                && known.writes.get(*at).copied() == Some(0)
                && !handed.contains(at)
                && !named_twice.contains(at)
                && !known.taken.contains(at)
                && known.reads.get(*at).is_some_and(|reads| *reads > 0)
                && read_in_loops.contains(at)
        })
        .collect()
}

/// The blocks inside a loop: for each jump back to a loop's start, every block that reaches the
/// jump without going through the start, and the start.
fn blocks_in_loops(function: &Function) -> HashSet<usize> {
    let mut coming_from: HashMap<usize, Vec<usize>> = HashMap::new();
    for (at, block) in function.blocks.iter().enumerate() {
        for target in block.end.targets() {
            coming_from.entry(target.0).or_default().push(at);
        }
    }
    let mut found = HashSet::new();
    let mut back: Vec<(usize, usize)> = crate::emit::back_edges(function).into_iter().collect();
    back.sort_unstable();
    for (from, start) in back {
        let mut seen = HashSet::from([start]);
        let mut waiting = vec![from];
        while let Some(at) = waiting.pop() {
            if seen.insert(at) {
                waiting.extend(coming_from.get(&at).into_iter().flatten().copied());
            }
        }
        found.extend(seen);
    }
    found
}

/// How many instructions after a read of `xs[i]` the read of `xs[i + 1]` is looked for, so a long
/// block of reads costs a few looks each rather than one for every instruction after it.
const ADJACENT_READS_APART: usize = 32;

/// Reads of a list's item at `i` followed in the same block by one at `i + 1`, with nothing
/// between them that could change the list or `i`: `starts[near]`, then `starts[near + 1]`. Both
/// are tested against one test that `i` is from 0 to below the length less one, which puts both
/// inside; only when it fails is each checked on its own, in order, so one outside fails as it
/// would alone. Two checks with two different failures were two compares and branches that LLVM
/// couldn't merge.
fn find_adjacent_reads(function: &Function, known: &mut Analysis) {
    for (at, block) in function.blocks.iter().enumerate() {
        for (first, inst) in block.insts.iter().enumerate() {
            let InstKind::GetIndex { object: Operand::Local(list), index: Operand::Local(low), .. } =
                &inst.kind
            else {
                continue;
            };
            let mut high = None;
            for (later, next) in block.insts.iter().enumerate().skip(first + 1).take(ADJACENT_READS_APART) {
                match &next.kind {
                    InstKind::Binary { dest, op: BinOp::Add, lhs, rhs }
                        if high.is_none()
                            && known.writes.get(dest.0) == Some(&1)
                            && matches!(
                                (lhs, rhs),
                                (Operand::Local(from), Operand::Int(1)) | (Operand::Int(1), Operand::Local(from))
                                    if from == low
                            ) =>
                    {
                        high = Some(*dest);
                    }
                    InstKind::GetIndex {
                        object: Operand::Local(again),
                        index: Operand::Local(at_high),
                        ..
                    } if again == list && Some(*at_high) == high => {
                        known.adjacent_reads.insert((at, first), (*low, *low));
                        known.adjacent_reads.insert((at, later), (*low, *at_high));
                        break;
                    }
                    other => {
                        // Only reads and arithmetic may come between, writing neither the list
                        // nor `i`.
                        let mut written = Vec::new();
                        visit(other, |_| {}, |local| written.push(local));
                        let quiet = matches!(
                            other,
                            InstKind::Binary { .. }
                                | InstKind::GetIndex { .. }
                                | InstKind::GetField { .. }
                                | InstKind::Copy { .. }
                        );
                        if !quiet || written.contains(list) || written.contains(low) {
                            break;
                        }
                    }
                }
            }
        }
    }
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
    known: &mut BTreeSet<Guard>,
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

/// Reads `order[at]` whose index counts up a `for` loop over `range(starts[a], starts[b])`, where
/// `starts` is a list parameter nothing here changes and `order` a list whose length nothing here
/// changes, as a counting sort's buckets are read. Where every item of `starts` is from 0 to
/// `len(order)`, which the function checks once on its way in (`Emit::check_item_ranges`), the
/// count starts at 0 or more, steps up by one, and stays below an item, so the read is inside
/// `order`. Where the check didn't hold, the read is checked as it always was.
fn find_ranged_reads(function: &Function, known: &mut Analysis) {
    let once = |local: usize| known.writes.get(local) == Some(&1) && !known.given_mut.contains(&local);
    // What writes each local written once.
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
    // A list parameter nothing here changes or gives another value, holding ints.
    let steady_items = |list: usize| {
        list < function.params
            && !function.mut_params.contains(&list)
            && known.writes.get(list) == Some(&0)
            && !known.changed.contains(&list)
            && !known.given_mut.contains(&list)
            && matches!(function.locals.get(list).map(|local| &local.ty), Some(Type::List(item)) if **item == Type::Int)
    };
    // The list a local's value was read out of as an item, through copies, if it's one that keeps
    // its items.
    let item_of = |mut local: usize| -> Option<usize> {
        for _ in 0..8 {
            if !once(local) {
                return None;
            }
            match made.get(&local)? {
                InstKind::Copy { value: Operand::Local(from), .. } => local = from.0,
                InstKind::GetIndex { object: Operand::Local(list), .. } if steady_items(list.0) => {
                    return Some(list.0);
                }
                _ => return None,
            }
        }
        None
    };
    let mut found: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    let mut counted_by: HashMap<(usize, usize), fennec_ir::Count> = HashMap::new();
    for count in function.counts.iter().filter(|count| !count.down) {
        let Some(bounds) = item_of(count.stop.0) else { continue };
        // The count's first value, the one write of it that isn't its step.
        let mut starts = function.blocks.iter().flat_map(|block| &block.insts).filter_map(|inst| match &inst
            .kind
        {
            InstKind::Copy { dest, value } if *dest == count.count => Some(value.clone()),
            InstKind::Binary { dest, op: BinOp::Add, lhs: Operand::Local(from), rhs: Operand::Int(1) }
                if *dest == count.count && *from == count.count =>
            {
                None
            }
            _ => {
                let mut writes = false;
                visit(&inst.kind, |_| {}, |local| writes |= local == count.count);
                writes.then_some(Operand::None)
            }
        });
        let (Some(first), None) = (starts.next(), starts.next()) else { continue };
        let from_bounds = match first {
            Operand::Int(start) => start >= 0,
            Operand::Local(start) => item_of(start.0) == Some(bounds),
            _ => false,
        };
        if !from_bounds {
            continue;
        }
        for (at, block) in function.blocks.iter().enumerate() {
            for (index, inst) in block.insts.iter().enumerate() {
                let InstKind::GetIndex {
                    object: Operand::Local(read), index: Operand::Local(position), ..
                } = &inst.kind
                else {
                    continue;
                };
                let counted = *position == count.count || known.below.get(&position.0) == Some(&count.stop);
                if counted
                    && known.below.get(&position.0) == Some(&count.stop)
                    && known.steady_lengths.contains(&read.0)
                {
                    found.insert((at, index), (bounds, read.0));
                    counted_by.insert((at, index), *count);
                }
            }
        }
    }
    // Each counted loop's blocks, its start among them, and the one block going into it.
    let back = crate::emit::back_edges(function);
    let mut ways_in: HashMap<usize, Vec<usize>> = HashMap::new();
    for (from, block) in function.blocks.iter().enumerate() {
        for to in block.end.targets() {
            ways_in.entry(to.0).or_default().push(from);
        }
    }
    let mut loops = Vec::new();
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
        let blocks = natural_loop(function, (header, then.0), &ways_in, &back);
        let outside: Vec<usize> = ways_in
            .get(&header)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .copied()
            .filter(|from| !blocks.contains(from))
            .collect();
        let [before] = outside.as_slice() else { continue };
        if function
            .blocks
            .get(*before)
            .is_some_and(|block| matches!(block.end, Terminator::Jump(to) if to.0 == header))
        {
            let early = leaves_early(function, header, &blocks);
            loops.push((blocks, *before, *count, early));
        }
    }
    // A read's check is made before the outermost loop it's in, where the loop's count says how
    // many turns it takes, so a check that costs more than they could is left out.
    let mut scans: Vec<RangeScan> = Vec::new();
    let mut reads = HashMap::new();
    let mut places: Vec<_> = found.into_iter().collect();
    places.sort_unstable();
    for ((at, index), (bounds, read)) in places {
        let Some((_, before, count, early)) = loops
            .iter()
            .filter(|(blocks, _, _, _)| blocks.contains(&at))
            .max_by_key(|(blocks, before, _, _)| (blocks.len(), usize::MAX - before))
        else {
            continue;
        };
        let scan = RangeScan {
            bounds,
            read,
            before: *before,
            count: count.count.0,
            stop: count.stop.0,
            early: *early,
        };
        let id = scans.iter().position(|known| *known == scan).unwrap_or_else(|| {
            scans.push(scan);
            scans.len() - 1
        });
        reads.insert((at, index), id);
    }
    known.versioned = find_versioned_loops(function, &reads, &counted_by, &ways_in, &back);
    known.ranged_reads = reads;
    known.range_scans = scans;
}

/// The most blocks, and instructions, a loop written twice may have.
const MOST_VERSIONED_BLOCKS: usize = 48;
const MOST_VERSIONED_INSTS: usize = 400;

/// The loops whose count indexes reads `find_ranged_reads` found, each to be written twice: as
/// today, and with those reads unchecked, the way in choosing by the flag of their check once a
/// time in rather than at every read. A loop is its start and every block that goes back to it
/// without passing through it; it needs one block going into it, by a plain jump, and to be small,
/// since it's code written twice. Only loops whose own reads all lean on one check: a read in its
/// blocks that leans on another goes by that one's flag in both copies.
///
/// The second copy is the first's text with what it defines renamed (`Emit::write_second_copies`),
/// so it's written with its ways out: every block only the loop's blocks lead to, as one ending
/// in a `break` or a `return` is.
fn find_versioned_loops(
    function: &Function,
    reads: &HashMap<(usize, usize), usize>,
    counted_by: &HashMap<(usize, usize), fennec_ir::Count>,
    ways_in: &HashMap<usize, Vec<usize>>,
    back: &HashSet<(usize, usize)>,
) -> Vec<Versioned> {
    let mut by_count: BTreeMap<usize, (fennec_ir::Count, BTreeSet<usize>)> = BTreeMap::new();
    let mut places: Vec<(&(usize, usize), &usize)> = reads.iter().collect();
    places.sort_unstable();
    for (place, id) in places {
        let Some(count) = counted_by.get(place) else { continue };
        by_count.entry(count.count.0).or_insert_with(|| (*count, BTreeSet::new())).1.insert(*id);
    }
    let mut found: Vec<Versioned> = Vec::new();
    for (count, scans) in by_count.into_values() {
        let [scan] = scans.into_iter().collect::<Vec<_>>()[..] else { continue };
        let held = Operand::Local(count.count);
        let header = function.blocks.iter().position(|block| {
            let Terminator::Branch { cond: Operand::Local(cond), .. } = &block.end else { return false };
            block.insts.iter().any(|inst| {
                matches!(&inst.kind, InstKind::Binary { dest, op: BinOp::Lt, lhs, rhs: Operand::Local(stop) }
                    if dest == cond && *lhs == held && *stop == count.stop)
            })
        });
        let Some(header) = header else { continue };
        let Some(Terminator::Branch { then, otherwise, .. }) =
            function.blocks.get(header).map(|block| &block.end)
        else {
            continue;
        };
        let natural = natural_loop(function, (header, then.0), ways_in, back);
        let into = ways_in.get(&header).map_or(&[][..], Vec::as_slice);
        let outside: Vec<usize> = into.iter().copied().filter(|from| !natural.contains(from)).collect();
        let [before] = outside[..] else { continue };
        let plain_way_in = function
            .blocks
            .get(before)
            .is_some_and(|block| matches!(block.end, Terminator::Jump(to) if to.0 == header));
        // Nothing but the way in goes into the loop's blocks from outside them.
        let closed = natural.iter().all(|at| {
            *at == header
                || ways_in.get(at).map_or(&[][..], Vec::as_slice).iter().all(|from| natural.contains(from))
        });
        // With its ways out: each block every way into which is from those written already, but
        // where the loop goes when its count reaches its stop, and what follows that.
        let mut blocks = natural.clone();
        let mut grew = true;
        while grew {
            grew = false;
            for at in 0..function.blocks.len() {
                let into = ways_in.get(&at).map_or(&[][..], Vec::as_slice);
                if at != otherwise.0
                    && !blocks.contains(&at)
                    && !into.is_empty()
                    && into.iter().all(|from| blocks.contains(from))
                {
                    blocks.insert(at);
                    grew = true;
                }
            }
        }
        let insts: usize =
            blocks.iter().filter_map(|at| function.blocks.get(*at)).map(|block| block.insts.len()).sum();
        if plain_way_in
            && closed
            && blocks.len() <= MOST_VERSIONED_BLOCKS
            && insts <= MOST_VERSIONED_INSTS
            && !found.iter().any(|other| other.blocks.iter().any(|at| blocks.contains(at)))
        {
            found.push(Versioned { header, before, blocks, scan });
        }
    }
    // A loop whose way in is a block of another written twice, as one in a way out of it is,
    // isn't written twice itself: each copy of the other would go into its copies, and its reads
    // go by their flag instead. Only the other's copies are then made from text holding it.
    let inside: Vec<bool> = found
        .iter()
        .map(|loop_| {
            found.iter().any(|other| other.header != loop_.header && other.blocks.contains(&loop_.before))
        })
        .collect();
    found.into_iter().zip(inside).filter_map(|(loop_, inside)| (!inside).then_some(loop_)).collect()
}

/// A loop's blocks: its start, and every block reached from its body that goes back to the start
/// without passing through it. A block that ends in a `break` or a `return` isn't one. It's found
/// back from the loop's own jumps back to its start (`back`, from [`crate::emit::back_edges`]),
/// not from every way into the start the body reaches: a `break` inside another loop reaches the
/// block going into this one by way of the other's, which would count it as one of this loop's
/// and leave it no way in.
fn natural_loop(
    function: &Function,
    (header, body): (usize, usize),
    ways_in: &HashMap<usize, Vec<usize>>,
    back: &HashSet<(usize, usize)>,
) -> BTreeSet<usize> {
    let reached = loop_blocks(function, header, body);
    let into = ways_in.get(&header).map_or(&[][..], Vec::as_slice);
    let mut blocks: BTreeSet<usize> = BTreeSet::from([header]);
    let mut waiting: Vec<usize> = into
        .iter()
        .copied()
        .filter(|from| reached.contains(from) && back.contains(&(*from, header)))
        .collect();
    while let Some(at) = waiting.pop() {
        if blocks.insert(at) {
            waiting.extend(
                ways_in
                    .get(&at)
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .copied()
                    .filter(|from| reached.contains(from)),
            );
        }
    }
    blocks
}

/// Whether a loop can be left other than by its start's test: a block in it going out of it, as a
/// `break` or a `return` does.
fn leaves_early(function: &Function, header: usize, blocks: &BTreeSet<usize>) -> bool {
    blocks.iter().filter(|at| **at != header).any(|at| {
        function.blocks.get(*at).is_some_and(|block| {
            matches!(block.end, Terminator::Return(_))
                || block.end.targets().iter().any(|target| !blocks.contains(&target.0))
        })
    })
}

/// A loop written twice (`find_versioned_loops`): its start, the one block going into it, its
/// blocks with its ways out, and the check of item ranges whose flag picks the copy with no checks
/// on the reads that lean on it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Versioned {
    pub(crate) header: usize,
    pub(crate) before: usize,
    pub(crate) blocks: BTreeSet<usize>,
    pub(crate) scan: usize,
}

/// A check, at the end of the block `before`, that every item of the list `bounds` is from 0 to
/// the length of `read`, made only when the loop that block goes into, counting `count` up to
/// `stop`, takes enough turns to pay for it (`find_ranged_reads`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RangeScan {
    pub(crate) bounds: usize,
    pub(crate) read: usize,
    pub(crate) before: usize,
    pub(crate) count: usize,
    pub(crate) stop: usize,
    /// Whether the loop can be left before its count reaches its stop, by a `break` or a `return`,
    /// so its count says only how many turns it may take: the check is then made only for a list
    /// short enough to cost little whatever it takes.
    pub(crate) early: bool,
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
fn find_slice_keys(function: &Function, in_entries: InEntries<'_>, known: &mut Analysis) {
    let insts: Vec<&InstKind> =
        function.blocks.iter().flat_map(|block| &block.insts).map(|inst| &inst.kind).collect();
    let steady = |operand: &Operand, known: &Analysis| match operand {
        Operand::Local(local) => known.writes.get(local.0).copied().unwrap_or(2) <= 1,
        Operand::Int(_) => true,
        _ => false,
    };
    // A dict that holds structs in its entries is left out: finding a slice's bytes where they
    // are, and adding them, are written for an entry of a key and one word, or a string's header.
    // Which dicts do is the layouts' to say: a tuple, an array or an `Id` is held in place as a
    // struct is, whatever it's written as.
    let str_keyed = |dict: &Operand| match dict {
        Operand::Local(local) => function.locals.get(local.0).is_some_and(|local| {
            matches!(&local.ty, Type::Dict(key, _) if **key == Type::Str) && !in_entries(&local.ty)
        }),
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

/// `text[a:b].split(x)` and `.find(x)`, with the slice read by that one call and nothing else, and
/// what it was made from unchanged after: for a string in a block, the runtime reads the range where
/// it is, as slicing would, so no string is made for the slice. Slicing never fails, so nothing a
/// program sees moves. `==` and `startswith` are left out: a slice of 23 bytes or fewer is made
/// natively, with no call, and compared in its words, which a call can't beat. A slice used as a
/// dict's key is left to `find_slice_keys`.
fn find_slice_ranges(function: &Function, known: &mut Analysis) {
    let insts: Vec<&InstKind> =
        function.blocks.iter().flat_map(|block| &block.insts).map(|inst| &inst.kind).collect();
    let steady = |operand: &Operand, known: &Analysis| match operand {
        Operand::Local(local) => known.writes.get(local.0).copied().unwrap_or(2) <= 1,
        Operand::Int(_) => true,
        _ => false,
    };
    for inst in &insts {
        let InstKind::Slice { dest: slice, object, start, end } = inst else { continue };
        let Operand::Local(text) = object else { continue };
        if known.unmade.contains(&slice.0)
            || function.locals.get(text.0).map(|local| &local.ty) != Some(&Type::Str)
            || known.writes.get(slice.0) != Some(&1)
            || !steady(object, known)
            || !start.iter().chain(end).all(|at| steady(at, known))
        {
            continue;
        }
        // Copied once to the local the call reads, or read as it is.
        let copied = insts.iter().find_map(|inst| match inst {
            InstKind::Copy { dest, value: Operand::Local(from) } if from == slice => Some(*dest),
            _ => None,
        });
        let read = match copied {
            Some(read) if known.reads.get(slice.0) == Some(&1) && known.writes.get(read.0) == Some(&1) => {
                read
            }
            Some(_) => continue,
            None => *slice,
        };
        let as_range = Operand::Local(read);
        let mut uses = 0;
        for inst in &insts {
            match inst {
                InstKind::Builtin { name, args, .. }
                    if matches!(name.as_str(), "str.split" | "str.find")
                        && args.first() == Some(&as_range)
                        && !args.iter().skip(1).any(|arg| *arg == as_range) =>
                {
                    uses += 1;
                }
                _ => {}
            }
        }
        if uses != 1 || known.reads.get(read.0).copied() != Some(1) {
            continue;
        }
        known.slice_ranges.insert(read.0, (object.clone(), start.clone(), end.clone()));
        known.unmade.insert(slice.0);
        known.unmade.insert(read.0);
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
            | InstKind::LockAll { .. }
            | InstKind::UnlockAll { .. }
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
        // Both texts are only read, so either may be the one looked into, or both.
        InstKind::Builtin { name, args, .. } if name == "intrinsics.text_same_at" => {
            (args.first() == Some(&held) || args.get(2) == Some(&held)) && args.get(1) != Some(&held)
        }
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
    // A lock's handle is where a place's path may start only when the lock saves what it changes,
    // so a take from it is put back by its log.
    let saving = crate::rollback::saving_handles(function);
    let reachable =
        |local: LocalId| !crate::rollback::is_lock_handle(function, local) || saving.contains(&local.0);
    for block in &function.blocks {
        // Where a borrowed list came from, for an item taken out of it: each local it was read out
        // of on the way, nearest first, with the path from it, to the root of the chain. A lock's
        // `held.value.named` is read out of `held` two fields up, as `self.named` is one up.
        let mut from_field: HashMap<usize, Vec<(LocalId, Vec<String>)>> = HashMap::new();
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
                let homes = from_field.get(&source.0).cloned().unwrap_or_else(|| vec![(source, Vec::new())]);
                let nearest = homes.first().map_or(source, |(at, _)| *at);
                for next in later {
                    match &next.kind {
                        InstKind::SetIndex { object, path, index: put_at, value: Operand::Local(put) }
                            if put_at == index
                                && *put == dest
                                && homes.iter().any(|(at, along)| at == object && along == path) =>
                        {
                            known.take_item(dest, (*object, path.clone()));
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
                        InstKind::SetIndex { object, path, .. } if *object != nearest && path.is_empty() => {}
                        other if is_quiet(other) => {}
                        _ => break,
                    }
                }
                continue;
            }
            let InstKind::GetField { field, .. } = &inst.kind else { continue };
            if known.borrowed.contains(&dest.0) {
                let mut homes = vec![(source, vec![field.clone()])];
                for (at, along) in from_field.get(&source.0).into_iter().flatten() {
                    if reachable(*at) {
                        homes.push((*at, along.iter().chain([field]).cloned().collect()));
                    }
                }
                from_field.insert(dest.0, homes);
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
        if !known.take_place(place.value, number) {
            continue;
        }
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
///
/// The IR works `x` out between the read and the append. Where that's more than a read, a string
/// being made, a call, a struct, the field is taken only when the append comes: nothing is out of
/// the root while `x` is made, so a failure there has nothing to put back, and the read before it
/// still fails first on a key that isn't there. That needs `x` to leave the root, the index and
/// what's read on the way as they are; anything else keeps the field's own copy, as before.
fn find_path_takes(function: &Function, changes: Changes<'_>, known: &mut Analysis) {
    let steady = |operand: &Operand, known: &Analysis| match operand {
        // A number or a string written out: a list's position, or a dict's key.
        Operand::Int(_) | Operand::Str(_) => true,
        Operand::Local(local) => known.writes.get(local.0).copied().unwrap_or(0) <= 1,
        _ => false,
    };
    // A lock's handle is a root only when the lock saves what it changes: the take asks its log.
    let saving = crate::rollback::saving_handles(function);
    for (block_at, block) in function.blocks.iter().enumerate() {
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
        'reads: for (at, inst) in block.insts.iter().enumerate() {
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
            // The list is the root itself, or reached from it by fields read one after another: a
            // struct's `self.named` one up, a lock's `held.value.named` two. Each local on the way
            // is written once.
            let (mut root, mut list_path, mut chain) = (list, Vec::new(), Vec::new());
            // Each step goes to a local written once before, so this ends; the bound is for safety,
            // as `root_of`'s is.
            let mut reached = false;
            for _ in 0..=function.locals.len() {
                match defined(root, at) {
                    Some((Some(step), up, None)) if known.writes.get(root.0) == Some(&1) => {
                        chain.push(root);
                        list_path.insert(0, step);
                        root = up;
                    }
                    None => {
                        reached = true;
                        break;
                    }
                    _ => continue 'reads,
                }
            }
            if !reached {
                continue;
            }
            if crate::rollback::is_lock_handle(function, root) && !saving.contains(&root.0) {
                continue;
            }
            if !steady(&index, known) || !known.borrowed.contains(&item.0) {
                continue;
            }
            // What's worked out for the append before it stands by: it may read the root, but
            // changes neither it nor the index, and touches nothing read on the way to the field.
            let index_local = match &index {
                Operand::Local(local) => Some(*local),
                _ => None,
            };
            let on_the_way: Vec<LocalId> = [*dest, *item].into_iter().chain(chain.iter().copied()).collect();
            let stands_by = |inst: &InstKind| {
                let (mut read, mut written) = (Vec::new(), Vec::new());
                visit(inst, |operand| read.push(operand.clone()), |local| written.push(local));
                let touched =
                    |local: LocalId| read.contains(&Operand::Local(local)) || written.contains(&local);
                let changed = |local: LocalId| {
                    written.contains(&local)
                        || may_change(inst, std::slice::from_ref(&local), changes, &Roots::new())
                };
                !on_the_way.iter().any(|local| touched(*local))
                    && !changed(root)
                    && !index_local.is_some_and(changed)
            };
            // Then: changes to it, the same place read again, and it put back there. The list is
            // read again by the same fields from the root, each step into a new local.
            let mut again: (Option<LocalId>, Option<LocalId>) = (list_path.is_empty().then_some(root), None);
            let mut read_again: HashMap<usize, Vec<String>> = HashMap::new();
            let mut put_in_item = false;
            // Where the first append is, and whether anything stood by before it.
            let (mut appended_at, mut waits) = (None, false);
            for (after, next) in block.insts.get(at + 1..).unwrap_or_default().iter().enumerate() {
                match &next.kind {
                    InstKind::Builtin { name, args, .. }
                        if name == "list.append"
                            && args.first() == Some(&Operand::Local(*dest))
                            && args.iter().skip(1).all(|arg| *arg != Operand::Local(root)) =>
                    {
                        appended_at = appended_at.or(Some(at + 1 + after));
                    }
                    // The place read again to put the field back, which comes after the append:
                    // before it, the same read is one the argument makes.
                    InstKind::GetField { dest: step, object: Operand::Local(from), field: name }
                        if appended_at.is_some()
                            && again.0.is_none()
                            && (*from == root || read_again.contains_key(&from.0))
                            && {
                                let mut along = read_again.get(&from.0).cloned().unwrap_or_default();
                                along.push(name.clone());
                                list_path.starts_with(&along)
                            } =>
                    {
                        let mut along = read_again.get(&from.0).cloned().unwrap_or_default();
                        along.push(name.clone());
                        if along == list_path {
                            again.0 = Some(*step);
                        } else {
                            read_again.insert(step.0, along);
                        }
                    }
                    InstKind::GetIndex { dest: item2, object: Operand::Local(from), index: at_again }
                        if appended_at.is_some()
                            && Some(*from) == again.0
                            && *at_again == index
                            && again.1.is_none() =>
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
                    // The item read again is taken too, with the same home: read with an owner of
                    // its own, it would hold the field's word from before the change, which a
                    // list that grew and moved has let go of.
                    InstKind::SetIndex { object, path, index: at_again, value: Operand::Local(put) }
                        if put_in_item
                            && *object == root
                            && *path == list_path
                            && *at_again == index
                            && Some(*put) == again.1
                            && known.items_taken.get(&put.0) == Some(&(root, list_path.clone())) =>
                    {
                        // A field that waited with nothing appended to it has nothing to be
                        // taken for, so it keeps its own copy.
                        if waits && appended_at.is_none() {
                            break;
                        }
                        known.taken.insert(dest.0);
                        known.path_taken.insert(dest.0, (root, list_path.clone(), index.clone()));
                        if waits && let Some(append) = appended_at {
                            known.path_waiting.insert(dest.0);
                            known.path_waits.insert((block_at, append), (*dest, *item, field.clone()));
                        }
                        break;
                    }
                    other if is_quiet(other) && !reads_local(other, root) && !reads_local(other, *dest) => {}
                    other if appended_at.is_none() && stands_by(other) => waits = true,
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
                && known.take_item(*dest, (root, path))
            {
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
                && read_before_put_back_again(function, (start, at), *item)
            {
                let read_taken = known.take_item(*read, (root, path.clone()));
                if known.take_item(*item, (root, path)) && read_taken {
                    known.put_back_reads.insert(item.0, *read);
                }
            }
        }
    }
}

/// Whether every way on from where `item` is put back reaches the read at `read` again, or leaves,
/// before it can come to the put-back again. `stays_apart` follows each way from the read only as
/// far as the put-back, so a loop around the put-back alone, as `while True:` with its way out
/// after it makes, would put the item back twice from one read: the second time into a slot
/// that may be gone, and with what the first put back changed in place since showing through.
fn read_before_put_back_again(function: &Function, read: (usize, usize), item: LocalId) -> bool {
    let is_back = |inst: &InstKind| matches!(inst, InstKind::SetIndex { value: Operand::Local(put), .. } if *put == item);
    let backs: Vec<(usize, usize)> = function
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(at, block)| {
            block
                .insts
                .iter()
                .enumerate()
                .filter(|(_, inst)| is_back(&inst.kind))
                .map(move |(index, _)| (at, index))
        })
        .collect();
    for (back_block, back_at) in backs {
        let mut seen = HashSet::new();
        let mut waiting = vec![(back_block, back_at + 1)];
        while let Some((at, first)) = waiting.pop() {
            let Some(block) = function.blocks.get(at) else { return false };
            let mut ended = false;
            for (index, inst) in block.insts.iter().enumerate().skip(first) {
                if (at, index) == read {
                    ended = true;
                    break;
                }
                if is_back(&inst.kind) {
                    return false;
                }
            }
            if ended {
                continue;
            }
            for target in block.end.targets() {
                if seen.insert(target.0) {
                    waiting.push((target.0, 0));
                }
            }
        }
    }
    true
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

/// A loop that changes lists or `Table`s held in locals, each of which nothing in the loop can
/// give another owner: one check on the way in that each is alone does for every change in the
/// loop, where today each change checks again.
#[derive(Debug, Hash)]
pub(crate) struct AloneInLoop {
    /// The block that goes into the loop, with a jump to its start that nothing else makes.
    pub(crate) entry: usize,
    /// The loop's blocks, its start among them.
    pub(crate) blocks: BTreeSet<usize>,
    /// The locals made alone on the way in.
    pub(crate) locals: Vec<LocalId>,
}

/// For each loop with one way in, a plain jump to its start, the lists and `Table`s held in
/// locals that it changes and that nothing in it can give another owner. A value gets another
/// owner by being copied, passed to a call or a closure, captured, put inside another value, or
/// yielded; a local written anew in the loop might hold a block another owner has; and a list a
/// lock's log may have marked never reads as alone. Reading items and fields, setting them, and
/// a collection's own builtins on it give it no owner, so a value made alone before the loop is
/// still alone at every change in it.
fn find_alone_in_loops(function: &Function, known: &mut Analysis) {
    let mut coming_from: HashMap<usize, Vec<usize>> = HashMap::new();
    for (at, block) in function.blocks.iter().enumerate() {
        for target in block.end.targets() {
            coming_from.entry(target.0).or_default().push(at);
        }
    }
    let mut back: Vec<(usize, usize)> = crate::emit::back_edges(function).into_iter().collect();
    back.sort_unstable();
    let mut starts: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (from, start) in back {
        starts.entry(start).or_default().push(from);
    }
    for (start, ends) in starts {
        let mut blocks = BTreeSet::from([start]);
        let mut waiting = ends;
        while let Some(at) = waiting.pop() {
            if blocks.insert(at) {
                waiting.extend(coming_from.get(&at).into_iter().flatten().copied());
            }
        }
        // Where control flow isn't reducible, as in a generator's resume, walking back from a
        // jump to the start can reach the function's first block without passing the start:
        // there's a way in that doesn't go through the entry, so the loop is left as it is.
        if blocks.contains(&0) {
            continue;
        }
        let outside: Vec<usize> = coming_from
            .get(&start)
            .into_iter()
            .flatten()
            .copied()
            .filter(|from| !blocks.contains(from))
            .collect();
        let [entry] = outside.as_slice() else { continue };
        if !matches!(function.blocks.get(*entry).map(|block| &block.end), Some(Terminator::Jump(to)) if to.0 == start)
        {
            continue;
        }
        let inside: Vec<&Block> = blocks.iter().filter_map(|at| function.blocks.get(*at)).collect();
        let locals: Vec<LocalId> = function
            .locals
            .iter()
            .enumerate()
            .filter(|(at, local)| {
                matches!(local.ty, Type::List(_) | Type::Table(_)) && !known.maybe_marked.contains(at)
            })
            .map(|(at, _)| LocalId(at))
            .filter(|held| {
                changes_in(&inside, known, *held)
                    && gives_no_owner(function, &inside, known, *held)
                    && !copy_read_in(function, &blocks, &inside, known, *held)
            })
            .collect();
        if !locals.is_empty() {
            known.alone_in_loops.push(AloneInLoop { entry: *entry, blocks, locals });
        }
    }
}

/// Whether a loop reads a copy of `held` made before it, as a loop through `items[k:]` holds the
/// list as it was while its body changes it. `held` then has another owner on every way in, so
/// making it alone there would copy all of it, where its first change in the loop does that work
/// anyway: an append to a list held twice grows it into a block of its own, with no copy made
/// first. So such a loop is left to check at each change. A local that only stands for `held`,
/// with no count of its own (see [`Analysis::alias`]), is no other owner.
fn copy_read_in(
    function: &Function,
    loop_blocks: &BTreeSet<usize>,
    inside: &[&Block],
    known: &Analysis,
    held: LocalId,
) -> bool {
    let mut copies = BTreeSet::from([held]);
    let mut grew = true;
    while grew {
        grew = false;
        let before: Vec<&Block> = function
            .blocks
            .iter()
            .enumerate()
            .filter(|(at, _)| !loop_blocks.contains(at))
            .map(|(_, block)| block)
            .collect();
        for inst in before.iter().flat_map(|block| &block.insts) {
            if let InstKind::Copy { dest, value: Operand::Local(from) } = &inst.kind
                && copies.contains(from)
                && known.alias(*dest).is_none()
                && copies.insert(*dest)
            {
                grew = true;
            }
        }
    }
    copies.remove(&held);
    inside.iter().any(|block| {
        copies.iter().any(|copy| {
            block.insts.iter().any(|inst| reads_local(&inst.kind, *copy)) || reads_in_end(&block.end, *copy)
        })
    })
}

/// The builtins on a list or a `Table` that give it no other owner, called on it: what each gives
/// back, if anything, is an item, a count, a flag, an `Id` or a list of those, never what it's
/// called on. A builtin added to either has to be added here to keep a loop's values alone.
const GIVE_NO_OWNER: [&str; 28] = [
    "list.append",
    "list.insert",
    "list.extend",
    "list.pop",
    "list.remove",
    "list.clear",
    "list.remove_where",
    "list.sort",
    "list.sort.key",
    "list.len",
    "list.contains",
    "list.reverse",
    "list.swap",
    "Table.add",
    "Table.remove",
    "Table.get",
    "Table.modify",
    "Table.remove_where",
    "Table.values",
    "Table.ids",
    "Table.items",
    "Table.clear",
    "Table.len",
    "Table.contains",
    "Table.slots",
    "Table.slot_live",
    "Table.slot_id",
    "Table.at_slot",
];

/// Whether a loop's blocks change `held`, or an item taken out of it.
fn changes_in(inside: &[&Block], known: &Analysis, held: LocalId) -> bool {
    inside.iter().flat_map(|block| &block.insts).any(|inst| match &inst.kind {
        InstKind::SetIndex { object, .. } | InstKind::SetField { object, .. } => *object == held,
        InstKind::Builtin { args, changes: true, .. } => args.first() == Some(&Operand::Local(held)),
        InstKind::GetIndex { dest, .. } | InstKind::Builtin { dest: Some(dest), .. } => {
            known.items_taken.get(&dest.0).is_some_and(|(root, _)| *root == held)
        }
        _ => false,
    })
}

/// Whether nothing in a loop's blocks can give `held`, or a local standing for it, another owner
/// (see [`find_alone_in_loops`]).
fn gives_no_owner(function: &Function, inside: &[&Block], known: &Analysis, held: LocalId) -> bool {
    let is_held = |local: LocalId| local == held || known.alias(local) == Some(held);
    let held_operand = |operand: &Operand| matches!(operand, Operand::Local(local) if is_held(*local));
    let ty = function.locals.get(held.0).map(|local| &local.ty);
    for block in inside {
        for inst in &block.insts {
            let allowed = match &inst.kind {
                InstKind::GetIndex { object, index, .. } => held_operand(object) && !held_operand(index),
                InstKind::GetField { object, .. } => held_operand(object),
                InstKind::SetIndex { object, index, value, .. } => {
                    is_held(*object) && !held_operand(index) && !held_operand(value)
                }
                InstKind::SetField { object, value, .. } => is_held(*object) && !held_operand(value),
                // A collection's own builtins on it, each of which gives back nothing that holds it.
                InstKind::Builtin { dest, name, args, .. } => {
                    args.first().is_some_and(held_operand)
                        && args.iter().skip(1).all(|arg| !held_operand(arg))
                        && GIVE_NO_OWNER.contains(&name.as_str())
                        && dest.is_none_or(|dest| {
                            !is_held(dest) && function.locals.get(dest.0).map(|local| &local.ty) != ty
                        })
                }
                _ => false,
            };
            let (mut read, mut written) = (false, false);
            visit(&inst.kind, |operand| read |= held_operand(operand), |dest| written |= is_held(dest));
            if (read || written) && !allowed {
                return false;
            }
        }
        // Handing it on out of the function leaves the loop; anything else that names it doesn't.
        let named = match &block.end {
            Terminator::Branch { cond: value, .. } | Terminator::NoMatch { value, .. } => held_operand(value),
            Terminator::Jump(_)
            | Terminator::Return(_)
            | Terminator::Stop { .. }
            | Terminator::Unreachable => false,
        };
        if named {
            return false;
        }
    }
    true
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

/// A list a loop fills, made once the loop's count is known (see `Analysis::reservations`).
#[derive(Clone, Debug, Hash)]
pub(crate) struct Reserved {
    pub(crate) list: usize,
    pub(crate) limit: Operand,
    /// The empty list made just before, and copied into `list`, when the list is made that way.
    pub(crate) copied_from: Option<usize>,
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
        // Every list the loop appends to, in any of its own blocks: one block a turn, as a walk
        // through a `Table`'s live slots appends past the test of whether a slot is live.
        let looped = loop_of(function, header_at.0, then.0);
        let appended: BTreeSet<usize> = looped
            .iter()
            .filter_map(|at| function.blocks.get(*at))
            .flat_map(|block| &block.insts)
            .filter_map(|inst| match &inst.kind {
                InstKind::Builtin { name, args, .. } if name == "list.append" => match args.first() {
                    Some(Operand::Local(list)) => Some(list.0),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        for list in appended {
            // What's known of a list while its loop fills it goes by its local: so it's known
            // only of a list nothing outside the loop appends to, before it or after, in another
            // loop or once on its own.
            if !appended_only_in(function, &looped, list)
                || !appended_every_turn(function, (header_at.0, then.0), &looped, list)
            {
                continue;
            }
            // Only a list made here, which has one owner, and empty: written straight in, or
            // made as a temporary nothing else reads and copied in, as `mut xs = list[int]()` is.
            // A constant list is one block every reader shares and nothing frees, so appending to
            // it with no count check would write into it. Nothing after it in the block may read it.
            let copied_from = match made_empty_here(block, known, list) {
                Some(MadeEmpty::Straight) => None,
                Some(MadeEmpty::CopiedFrom(temp)) => Some(temp),
                None => continue,
            };
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
            known.reservations.entry(at).or_default().push(Reserved {
                list,
                limit: limit.clone(),
                copied_from,
            });
            if let Some(count) = zeros_counted {
                known.zero_filled.insert(list);
                known.zero_counts.insert(list, count);
            }
        }
    }
}

/// Whether `inst` appends to `list`.
fn appends_to(inst: &InstKind, list: usize) -> bool {
    matches!(inst, InstKind::Builtin { name, args, .. }
        if name == "list.append" && args.first() == Some(&Operand::Local(LocalId(list))))
}

/// Whether every append to `list` in the function is in one of the loop's own blocks.
fn appended_only_in(function: &Function, looped: &[usize], list: usize) -> bool {
    function.blocks.iter().enumerate().all(|(at, block)| {
        looped.contains(&at) || !block.insts.iter().any(|inst| appends_to(&inst.kind, list))
    })
}

/// Whether every turn of the loop at `header_at` appends to `list`, but those a walk through a
/// `Table` steps over for an empty slot: then room for every turn is about as much as the list
/// comes to hold. A turn past a filter or a `continue` may add nothing, and room made for every
/// one of a long count would be a great deal of memory for a list that may stay short.
fn appended_every_turn(
    function: &Function,
    (header_at, body_at): (usize, usize),
    looped: &[usize],
    list: usize,
) -> bool {
    let slot_live = |cond: &Operand| {
        function.blocks.iter().flat_map(|block| &block.insts).any(|inst| {
            matches!(&inst.kind, InstKind::Builtin { dest: Some(dest), name, .. }
                if name == "Table.slot_live" && *cond == Operand::Local(*dest))
        })
    };
    // From the body, every way back round to the header without an append. A way out of the
    // loop isn't a turn: it appends no more than the turns before it.
    let mut seen = HashSet::new();
    let mut waiting = vec![body_at];
    while let Some(at) = waiting.pop() {
        if at == header_at {
            return false;
        }
        let Some(block) = function.blocks.get(at) else { continue };
        if !looped.contains(&at)
            || block.insts.iter().any(|inst| appends_to(&inst.kind, list))
            || !seen.insert(at)
        {
            continue;
        }
        match &block.end {
            Terminator::Branch { cond, then, .. } if slot_live(cond) => waiting.push(then.0),
            end => waiting.extend(end.targets().iter().map(|next| next.0)),
        }
    }
    true
}

/// How a list is made empty in the block before the loop that fills it.
enum MadeEmpty {
    /// Straight into the list.
    Straight,
    /// Into this temporary, copied into the list.
    CopiedFrom(usize),
}

/// Whether `list` is made empty in `block`, straight in or through a temporary copied in, with
/// nothing after that in the block reading either.
fn made_empty_here(block: &Block, known: &Analysis, list: usize) -> Option<MadeEmpty> {
    let made = |at: usize| {
        block.insts.iter().position(
            |inst| matches!(&inst.kind, InstKind::MakeList { dest, items } if dest.0 == at && items.is_empty()),
        )
    };
    let (from, made_at) = if let Some(made_at) = made(list) {
        (None, made_at)
    } else {
        let (copy_at, temp) = block.insts.iter().enumerate().find_map(|(at, inst)| match &inst.kind {
            InstKind::Copy { dest, value: Operand::Local(temp) } if dest.0 == list => Some((at, temp.0)),
            _ => None,
        })?;
        let made_at = made(temp).filter(|made_at| *made_at < copy_at)?;
        if known.reads.get(temp).copied() != Some(1) {
            return None;
        }
        (Some(temp), made_at)
    };
    let mentioned = |inst: &InstKind| {
        let mut found = false;
        visit(
            inst,
            |operand| {
                found |= matches!(operand, Operand::Local(local) if local.0 == list || Some(local.0) == from);
            },
            |_| {},
        );
        found
    };
    let read_after = block.insts.iter().skip(made_at + 1).any(|inst| {
        let copy_in = matches!(&inst.kind, InstKind::Copy { dest, .. } if dest.0 == list);
        !copy_in && mentioned(&inst.kind)
    });
    (!read_after).then_some(from.map_or(MadeEmpty::Straight, MadeEmpty::CopiedFrom))
}

/// What `appends_within_room` found: the appends stay within the room, and whether the loop
/// leaves only by its header, to the block after it.
enum Within {
    Room,
    RoomLeavingTo(usize),
}

/// The blocks of the loop at `header_at` whose body starts at `body_at`: those the body reaches
/// without going through the header again, that lead back to it, and the header. A loop around
/// this one is left out: it isn't this loop's turn.
fn loop_of(function: &Function, header_at: usize, body_at: usize) -> Vec<usize> {
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
    let mut looped: Vec<usize> = ahead.intersection(&behind).copied().chain([header_at]).collect();
    looped.sort_unstable();
    looped.dedup();
    looped
}

/// Whether block `at` of a loop can come round to itself without going through the loop's
/// header: it's in a loop inside that one.
fn turns_inside(function: &Function, looped: &[usize], (header_at, at): (usize, usize)) -> bool {
    let mut seen = HashSet::new();
    let mut waiting: Vec<usize> = function
        .blocks
        .get(at)
        .map(|block| block.end.targets().iter().map(|next| next.0).collect())
        .unwrap_or_default();
    while let Some(next) = waiting.pop() {
        if next == at {
            return true;
        }
        if next == header_at || !looped.contains(&next) || !seen.insert(next) {
            continue;
        }
        if let Some(block) = function.blocks.get(next) {
            waiting.extend(block.end.targets().iter().map(|target| target.0));
        }
    }
    false
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
    let looped = loop_of(function, header_at, body_at);
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
            // In any of the loop's own blocks, but in none it can come back to without the header:
            // a block in a loop inside this one could append more than once a turn.
            if !appended || turns_inside(function, &looped, (header_at, *at)) {
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
            path_waits: HashMap::new(),
            path_waiting: HashSet::new(),
            loop_taken: HashSet::new(),
            places_taken: HashMap::new(),
            place_parts: HashMap::new(),
            place_paths: HashMap::new(),
            places_waiting: HashSet::new(),
            field_of: HashMap::new(),
            slice_keys: HashMap::new(),
            slice_ranges: HashMap::new(),
            unmade: HashSet::new(),
            aliases: HashMap::new(),
            alone_in_loops: Vec::new(),
            steady_lengths: HashSet::new(),
            adjacent_reads: HashMap::new(),
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
            ranged_reads: HashMap::new(),
            range_scans: Vec::new(),
            versioned: Vec::new(),
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

    /// The list whose items bound the index of the read at this block and place in it, and the
    /// list read, if the read is inside wherever the function's check of those items held.
    /// The loop written twice that a block is in, if it's in one.
    pub(crate) fn versioned_at(&self, block: usize) -> Option<&Versioned> {
        self.versioned.iter().find(|loop_| loop_.blocks.contains(&block))
    }

    pub(crate) fn ranged_read(&self, at: (usize, usize)) -> Option<(usize, RangeScan)> {
        let id = *self.ranged_reads.get(&at)?;
        Some((id, *self.range_scans.get(id)?))
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

    /// Whether a field taken out by [`Self::path_home`] is taken only when its append comes.
    pub(crate) fn path_waits(&self, local: LocalId) -> bool {
        self.path_waiting.contains(&local.0)
    }

    /// The field taken out just before the instruction at `at`, an append it waited for: its
    /// local, the item it's read from and the field.
    pub(crate) fn path_due(&self, at: (usize, usize)) -> Option<&(LocalId, LocalId, String)> {
        self.path_waits.get(&at)
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

    /// Notes an item taken out of a list to change, held in `local` until it's put back to `home`,
    /// the list's root and the path to it, if `local` has one write. The emitter names what's known
    /// of the item by where it came from (`taken_items`), noting it as it writes that one write, so
    /// every read of the local sees the note: its one write comes before each of them, in the
    /// blocks as they run and as they're written. A local written twice would have reads that only
    /// the other write reaches, so it isn't noted, whatever a caller has checked.
    fn take_item(&mut self, local: LocalId, home: (LocalId, Vec<String>)) -> bool {
        if self.writes.get(local.0) != Some(&1) {
            return false;
        }
        self.taken.insert(local.0);
        self.items_taken.insert(local.0, home);
        true
    }

    /// Notes the place numbered `number` taken out for a call, held in `value`, if `value` has one
    /// write: for the same reason as [`Self::take_item`], since the emitter notes a place reached
    /// through a list's item as it writes it (`walk_place`).
    fn take_place(&mut self, value: LocalId, number: usize) -> bool {
        if self.writes.get(value.0) != Some(&1) {
            return false;
        }
        self.places_taken.insert(value.0, number);
        self.taken.insert(value.0);
        true
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
        // A bool or a number holds no block, so it leans on nothing, however it was read: a
        // match binding whose `bool` field was read first, with no owner of its own, was counted
        // as leaned on and copied into a `mut` where it could be moved, and every change then
        // copied its lists (#1953).
        if matches!(
            function.locals.get(local).map(|held| &held.ty),
            Some(Type::Unit | Type::None | Type::Bool | Type::Int | Type::Float)
        ) {
            continue;
        }
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
        // `u8(x)` stops the program when `x` doesn't fit, and `f32(x)` when a float is past an
        // `f32`'s largest: never one written out that fits, as `f32(9.8)`.
        || fennec_check::Num::named(name).is_some_and(|num| {
            !num.is_float()
                || match args.first() {
                    Some(Operand::Float(x)) => fennec_check::past_f32(*x),
                    first => first.and_then(|first| function.type_of(first)) == Some(Type::Float),
                }
        })
        // A stop can cut a channel's wait short.
        || matches!(
            name,
            "intrinsics.channel_new" | "intrinsics.channel_new_every" | "intrinsics.channel_send" | "intrinsics.channel_take"
        )
        // A launch fails with no thread to run on, and a wait for a task can be cut short.
        || matches!(name, "intrinsics.launch" | "intrinsics.launch_take")
        // The standard library's `stop` ends the program as a failure does, passed up through
        // whatever called it.
        || name == "intrinsics.stop"
        || is_socket_method(name)
        // Sorting what isn't a plain number, string or bool orders by its type's own `<`, which
        // can fail.
        || (name == "sorted"
            && args.first().and_then(|list| function.type_of(list)).is_some_and(|ty| {
                !matches!(&ty, Type::List(item) if matches!(**item, Type::Int | Type::Float | Type::Str | Type::Bool))
            }))
        // `bytes(list)` fails on an item that isn't a byte, which `bytes()` and `bytes([])` can't
        // have.
        || (name == "bytes" && !makes_no_bytes(function, args))
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
                    | "main.result"
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

/// Whether `bytes(...)` given `args` makes empty bytes, so can't fail: given nothing, or a list
/// written once as `[]` and read only by the `bytes(...)`, as `bytes([])` is, in a constant kept
/// for the whole run too. A list from anywhere else may hold a number that isn't a byte. One
/// written out of bytes can't either, but the runtime's call for a list says so by failing, so
/// it's left to that.
pub(crate) fn makes_no_bytes(function: &Function, args: &[Operand]) -> bool {
    let list = match args {
        [] => return true,
        [Operand::Local(list)] => *list,
        _ => return false,
    };
    let mut writes = function.blocks.iter().flat_map(|block| &block.insts).filter(|inst| {
        let mut writes = false;
        visit(&inst.kind, |_| {}, |local| writes |= local == list);
        writes
    });
    let (Some(write), None) = (writes.next(), writes.next()) else { return false };
    // Read by nothing but the `bytes(...)` itself, so nothing filled it after it was made: a
    // comprehension's list is made empty and then appended to, which is a read, not a write.
    let reads: usize = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .map(|inst| {
            let mut reads = 0;
            visit(&inst.kind, |operand| reads += usize::from(*operand == Operand::Local(list)), |_| {});
            reads
        })
        .sum();
    let ends_with_it = function.blocks.iter().any(|block| match &block.end {
        Terminator::Return(Some(value)) | Terminator::Branch { cond: value, .. } => {
            *value == Operand::Local(list)
        }
        _ => false,
    });
    match &write.kind {
        InstKind::MakeList { items, .. } | InstKind::ConstantList { items, .. } => {
            items.is_empty() && reads == 1 && !ends_with_it
        }
        _ => false,
    }
}

/// Whether `function` can fail of itself, rather than by what it calls failing, in a program that
/// uses the runtime's threads when `threads` says so.
fn fails_itself(function: &Function, known: &Analysis, threads: bool) -> bool {
    // What's worked out for a local of a sized type is checked to fit it.
    let sized =
        |dest: &LocalId| matches!(function.locals.get(dest.0).map(|local| &local.ty), Some(Type::Sized(_)));
    // Where other threads may run, a `shared` value is made a table of its own, with a copy of all
    // it was given (`fennec_shared_new`), and that copy can fail, as any copy can.
    let shared = |dest: &LocalId| {
        matches!(
            function.locals.get(dest.0).map(|local| &local.ty),
            Some(Type::Named(name)) if name.starts_with(crate::rollback::LOCKED)
        )
    };
    let inst_fails = |inst: &InstKind| match inst {
        InstKind::Binary { dest, op, .. }
            if sized(dest) && !matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) =>
        {
            true
        }
        InstKind::Unary { dest, op: UnaryOp::Neg, .. } if sized(dest) => true,
        InstKind::Make { dest, .. } if threads && shared(dest) => true,
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

/// What's worked out of the whole program, for what looks at it beside a build, as `checks_left`
/// and `copies_made` do: it uses no analysis kept from a build and keeps none (see `kept`).
pub(crate) fn whole(program: &Program) -> Whole<'_> {
    whole_keeping(program, &crate::kept::Keeping::off()).0
}

/// [`whole`] for a build, using again what the builds before in this process kept and keeping
/// its own (see `kept`), with the names of the functions whose analysis used again isn't what
/// analyzing them again gives, on a build that checks them: none on any other.
pub(crate) fn whole_kept(program: &Program) -> (Whole<'_>, Vec<String>) {
    let keeping = crate::kept::Keeping::start();
    let found = whole_keeping(program, &keeping);
    crate::kept::note_used_again(keeping.used_again(), found.0.analyses.len());
    // A kept analysis that analyzing again doesn't give: nothing is handed on, the wrong one least
    // of all, so the build after this one works every analysis out.
    if found.1.is_empty() {
        keeping.finish();
    }
    found
}

/// Each function's parameters' ranges, by its name.
type ParamRanges<'p> = HashMap<&'p str, Vec<Range>>;

/// What the check after a build needs to analyze again what the build used again, taken while the
/// build has it (`kept::AnalysesCheck`): each function checked, with its parameters'
/// ranges and the key of the analysis used, and what `analyze` is told of the rest of the program.
/// The functions checked, and a few words for each other: far less than the program.
pub(crate) struct CheckPack {
    checked: Vec<(Function, Vec<Range>, Key)>,
    /// The parameters each function changes, for those that change any.
    changes: HashMap<String, Vec<usize>>,
    /// Which parameters each function keeps.
    kept: HashMap<String, Vec<bool>>,
    /// Whether each kind of dict holds its values in its entries.
    held: Vec<(Type, bool)>,
}

impl CheckPack {
    /// What's checked of `checked`, functions of `functions`, as analyzing them was told:
    /// `kept` and `params` say which parameters each callee keeps and each function's ranges, and
    /// `held` which dicts hold their values in their entries.
    fn of(
        checked: &[(String, Key)],
        functions: &HashMap<&str, &Function>,
        (kept, params): (&HashMap<&str, Vec<bool>>, &ParamRanges<'_>),
        held: &[(&Type, bool)],
    ) -> CheckPack {
        CheckPack {
            checked: checked
                .iter()
                .filter_map(|(name, key)| {
                    let function = (*functions.get(name.as_str())?).clone();
                    Some((function, params.get(name.as_str()).cloned().unwrap_or_default(), *key))
                })
                .collect(),
            changes: functions
                .iter()
                .filter(|(_, function)| !function.mut_params.is_empty())
                .map(|(name, function)| ((*name).to_owned(), function.mut_params.clone()))
                .collect(),
            kept: kept.iter().map(|(name, kept)| ((*name).to_owned(), kept.clone())).collect(),
            held: held.iter().map(|(ty, held)| ((*ty).clone(), *held)).collect(),
        }
    }

    /// Whether there's anything to check.
    pub(crate) fn is_empty(&self) -> bool {
        self.checked.is_empty()
    }

    /// Each function checked, by its name, with the key of the analysis that was used.
    pub(crate) fn checked(&self) -> impl Iterator<Item = (&str, Key)> {
        self.checked.iter().map(|(function, _, key)| (function.name.as_str(), *key))
    }

    /// Those whose analysis worked out afresh has another key than the one used, sorted.
    pub(crate) fn differ(&self) -> Vec<String> {
        let changes = |callee: &str| self.changes.get(callee).cloned().unwrap_or_default();
        let keeps = |callee: &str, at: usize| {
            self.kept.get(callee).is_none_or(|params| params.get(at).copied().unwrap_or(true))
        };
        let in_entries = |dict: &Type| {
            self.held
                .iter()
                .find(|(ty, _)| ty == dict)
                .map_or_else(fennec_layout::dict_values_in_place, |(_, held)| *held)
        };
        let mut differ: Vec<String> = self
            .checked
            .iter()
            .filter(|(function, given, key)| {
                analysis_key(&analyze(function, &changes, &keeps, given, &in_entries)) != *key
            })
            .map(|(function, ..)| function.name.clone())
            .collect();
        differ.sort();
        differ
    }
}

/// The functions in `used`, each with the key of the analysis a build used again for it, whose
/// analysis worked out afresh now has another key, sorted, as the check after a build finds them.
/// Only `kept`'s tests use it, which don't run on Windows.
#[cfg(all(test, not(windows)))]
pub(crate) fn analyses_again(program: &Program, used: &[(String, Key)]) -> Vec<String> {
    let functions: HashMap<&str, &Function> = program
        .functions
        .iter()
        .chain(&program.constants)
        .map(|function| (function.name.as_str(), function))
        .collect();
    let changes = |callee: &str| functions.get(callee).map_or_else(Vec::new, |f| f.mut_params.clone());
    let kept = parameters_kept(&functions);
    let keeps = |callee: &str, at: usize| {
        kept.get(callee).is_none_or(|params| params.get(at).copied().unwrap_or(true))
    };
    let params = param_ranges(program, &changes, &keeps);
    let layouts = crate::layouts_of(program).ok();
    let held = in_entries_of(program, layouts.as_ref());
    CheckPack::of(used, &functions, (&kept, &params), &held).differ()
}

/// [`whole_kept`], using again what `keeping` found and keeping in it what's worked out.
pub(crate) fn whole_keeping<'p>(
    program: &'p Program,
    keeping: &crate::kept::Keeping,
) -> (Whole<'p>, Vec<String>) {
    let every = || program.functions.iter().chain(&program.constants);
    let functions: HashMap<&str, &Function> =
        every().map(|function| (function.name.as_str(), function)).collect();
    // What needs only the program is worked out on threads of its own beside the rest, each giving
    // what it would on its own. A panic in one is the build's, as it would be on this thread.
    let whole = std::thread::scope(|scope| {
        let signs = scope.spawn(|| crate::signs::non_negative(program));
        let builders = scope.spawn(|| crate::builders::builders(program, &functions));
        let tasks = scope.spawn(|| {
            let tasks = crate::ticks::tasks(program, &functions);
            let recurring = crate::ticks::recurring(program, &functions, &tasks);
            (tasks, recurring)
        });
        let layouts = scope.spawn(|| crate::layouts_of(program).ok());
        let changes = |callee: &str| functions.get(callee).map_or_else(Vec::new, |f| f.mut_params.clone());
        let kept = parameters_kept(&functions);
        let keeps = |callee: &str, at: usize| {
            kept.get(callee).is_none_or(|params| params.get(at).copied().unwrap_or(true))
        };
        let params = param_ranges(program, &changes, &keeps);
        let layouts = joined(layouts);
        let (tasks, recurring) = joined(tasks);
        let held = in_entries_of(program, layouts.as_ref());
        let abandons = program.generators.iter().any(|def| def.abandon.is_some());
        let loud = if abandons { lets_go_loudly(program, layouts.as_ref()) } else { HashSet::new() };
        // Without layouts, which a program that builds always has, any dict that isn't of plain
        // words may hold its values in its entries.
        let in_entries = |dict: &Type| {
            held.iter()
                .find(|(ty, _)| *ty == dict)
                .map_or_else(fennec_layout::dict_values_in_place, |(_, held)| *held)
        };
        let (analyses, (ir_keys, analysis_keys), differ) =
            analyzed(program, (&changes, &keeps), &params, &in_entries, keeping);
        // What's checked after the build of what it used again goes with what analyzing it is told,
        // taken now, while the build has it, rather than the program.
        if keeping.checks_after() {
            let (checked, _) = crate::kept::to_check_after(program, keeping.take_used());
            crate::kept::note_pack(CheckPack::of(&checked, &functions, (&kept, &params), &held));
        }
        let mut generators = HashMap::new();
        for def in &program.generators {
            generators.insert(def.name.as_str(), def);
            generators.insert(def.resume.as_str(), def);
            if let Some(abandon) = &def.abandon {
                generators.insert(abandon.as_str(), def);
            }
        }
        // A generator's functions are called through its block, so all the same way: with the flag.
        let threads = crate::parallel::uses_parallel(program);
        let mut can_fail: HashSet<&str> = every()
            .filter(|function| {
                loud.contains(function.name.as_str())
                    || analyses
                        .get(function.name.as_str())
                        .is_none_or(|known| fails_itself(function, known, threads))
            })
            .map(|function| function.name.as_str())
            .collect();
        for def in &program.generators {
            can_fail.insert(def.resume.as_str());
            can_fail.extend(def.abandon.as_deref());
        }
        let (settles, lengths) = std::thread::scope(|inner| {
            let lengths = inner.spawn(|| summarized(&functions, &analyses, Tracked::Lengths));
            (summarized(&functions, &analyses, Tracked::Owners), joined(lengths))
        });
        let under_locks = crate::rollback::changing_under_locks(&functions);
        let needs = needed(program, &functions, &analyses, &under_locks);
        let constants = every()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.insts)
            .filter_map(|inst| match &inst.kind {
                InstKind::Constant { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let on_workers = crate::parallel::run_on_workers(program);
        let made = Whole {
            // Put in below, once no thread borrows them.
            functions: HashMap::new(),
            analyses,
            ir_keys,
            analysis_keys,
            can_fail,
            generators,
            settles,
            lengths,
            builders: joined(builders),
            non_negative: joined(signs),
            needs,
            under_locks,
            constants,
            on_workers,
            cells: crate::parallel::has_cells(program),
            tasks,
            recurring,
            runtime_closures: crate::closures::runtime_holds_closures(program),
            tested: program.functions.iter().any(|function| crate::emit::is_test(&function.name)),
            abandons,
        };
        (made, differ)
    });
    let (made, differ) = whole;
    (Whole { functions, ..made }, differ)
}

/// What [`analyzed`] gives: each function's analysis, each function's IR key and its analysis's
/// key where the build keeps its analyses, and the functions whose analysis used again isn't what
/// analyzing it again gives.
type Analyzed<'p> = (HashMap<&'p str, std::sync::Arc<Analysis>>, Keys<'p>, Vec<String>);

/// One function's analysis as [`analyzed`] finds it: with its IR key and the analysis's key where
/// it's kept, and whether it was used again.
type Found<'p> = (&'p str, std::sync::Arc<Analysis>, Option<(Key, Key)>, bool);

/// Each function's IR key and its analysis's key, where a build keeps its analyses.
type Keys<'p> = (HashMap<&'p str, Key>, HashMap<&'p str, Key>);

/// What a thread of [`whole`]'s gave, or its panic, carried on in this thread.
fn joined<T>(thread: std::thread::ScopedJoinHandle<'_, T>) -> T {
    thread.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Whether each `dict[str, _]` a function of the program has a local of holds structs in its
/// entries, as `layouts` lay it out, asked here before the analyses run on threads: asking the
/// layouts may number a kind of block, which only one thread can do. `None` without layouts.
fn in_entries_of<'p>(
    program: &'p Program,
    layouts: Option<&fennec_layout::Layouts>,
) -> Vec<(&'p Type, bool)> {
    let Some(layouts) = layouts else { return Vec::new() };
    let mut held: Vec<(&Type, bool)> = Vec::new();
    let locals = program.functions.iter().chain(&program.constants).flat_map(|function| &function.locals);
    for local in locals {
        let keyed_by_text = matches!(&local.ty, Type::Dict(key, _) if **key == Type::Str);
        if keyed_by_text && !held.iter().any(|(ty, _)| **ty == local.ty) {
            // A string held in the entry as its header is found and added by a slice's bytes
            // too; a struct held there isn't.
            let in_place = match crate::kinds::Kind::of(layouts, &local.ty) {
                Ok(crate::kinds::Kind::Dict(_, fennec_rt::embed::dicts::Word::InPlace(number))) => {
                    !matches!(layouts.dict_value(number), Some(fennec_layout::Ty::Str))
                }
                _ => false,
            };
            held.push((&local.ty, in_place));
        }
    }
    held
}

/// The functions with a local whose value letting go of may run program code: a generator that
/// closes something when it's let go partway, or what may hold or reach one (see
/// `dying::lets_go_quietly`). Such a function can fail, as that code can, so it's given the flag:
/// a free it makes is checked right after, and a launched task whose free fails stops there.
/// Asked here, on one thread, as `in_entries_of` asks, and every function is one without layouts.
fn lets_go_loudly<'p>(program: &'p Program, layouts: Option<&fennec_layout::Layouts>) -> HashSet<&'p str> {
    let every = program.functions.iter().chain(&program.constants);
    let Some(layouts) = layouts else { return every.map(|function| function.name.as_str()).collect() };
    // Each type asked once: a program has few, over many locals.
    let mut asked: Vec<(&Type, bool)> = Vec::new();
    let mut quiet = |ty: &'p Type| {
        if let Some((_, quiet)) = asked.iter().find(|(known, _)| *known == ty) {
            return *quiet;
        }
        let quiet = crate::kinds::Kind::of(layouts, ty)
            .is_ok_and(|kind| crate::dying::lets_go_quietly(layouts, &kind));
        asked.push((ty, quiet));
        quiet
    };
    let mut loud = HashSet::new();
    for function in every {
        let lets_go_loudly = function.locals.iter().any(|local| !quiet(&local.ty));
        if lets_go_loudly {
            loud.insert(function.name.as_str());
        }
    }
    loud
}

/// Which of a callee's parameters a call may change, as the analyses on threads ask it.
type ChangesOn<'f> = &'f (dyn Fn(&str) -> Vec<usize> + Sync);

/// Whether a callee keeps the parameter at a place, as the analyses on threads ask it.
type KeepsOn<'f> = &'f (dyn Fn(&str, usize) -> bool + Sync);

/// Every function's [`analyze`], on as many threads as a build compiles with: each depends only
/// on its function and what it's given here, so the order they run in changes nothing.
/// Every function's own analysis, on as many threads as the build has, each used again from the
/// build before when its function and everything it asked of the program are as they were (see
/// `kept`), with the names of those used again that aren't what analyzing them again gives, on a
/// build that checks.
fn analyzed<'p>(
    program: &'p Program,
    (changes, keeps): (ChangesOn<'_>, KeepsOn<'_>),
    params: &HashMap<&str, Vec<Range>>,
    in_entries: &(dyn Fn(&Type) -> bool + Sync),
    keeping: &crate::kept::Keeping,
) -> Analyzed<'p> {
    use std::sync::Arc;
    let all: Vec<&Function> = program.functions.iter().chain(&program.constants).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = crate::build::workers().min(all.len()).max(1);
    let found: Vec<Found<'p>> = std::thread::scope(|scope| {
        let parts: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(function) = all.get(at) else { break };
                        let given = params.get(function.name.as_str()).map_or(&[][..], Vec::as_slice);
                        // A build that keeps nothing analyzes each function as it always did.
                        if !keeping.keeps() {
                            let analysis = Arc::new(analyze(function, changes, keeps, given, in_entries));
                            mine.push((function.name.as_str(), analysis, None, false));
                            continue;
                        }
                        let ir = crate::keys::ir_key(function);
                        let told: crate::kept::Told<'_> = (changes, keeps, in_entries);
                        if let Some((analysis, key)) = keeping.usable(function, (&ir, given), told) {
                            keeping.keep_again(function);
                            mine.push((function.name.as_str(), analysis, Some((ir, key)), true));
                            continue;
                        }
                        let asking = crate::kept::Asking::new();
                        let noted_changes = |callee: &str| {
                            let told = changes(callee);
                            asking.changes(callee, &told);
                            told
                        };
                        let noted_keeps = |callee: &str, at: usize| {
                            let told = keeps(callee, at);
                            asking.keeps(callee, at, told);
                            told
                        };
                        let noted_in_entries = |dict: &Type| {
                            let told = in_entries(dict);
                            asking.in_entries(dict, told);
                            told
                        };
                        let analysis = Arc::new(analyze(
                            function,
                            &noted_changes,
                            &noted_keeps,
                            given,
                            &noted_in_entries,
                        ));
                        let key = analysis_key(&analysis);
                        keeping.keep(function, (ir, given), asking.into_asked(), (&analysis, key));
                        mine.push((function.name.as_str(), analysis, Some((ir, key)), false));
                    }
                    mine
                })
            })
            .collect();
        parts.into_iter().flat_map(joined).collect()
    });
    // Whether this build checks what it used again: picked by the program's functions as they are.
    let inputs = found.iter().fold(0_u64, |inputs, (_, _, ir, _)| {
        let Some((ir, _)) = ir else { return inputs };
        let mut word = [0_u8; 8];
        word.copy_from_slice(ir.get(..8).unwrap_or(&[0; 8]));
        inputs ^ u64::from_le_bytes(word)
    });
    let mut differ = Vec::new();
    if found.iter().any(|(.., used_again)| *used_again)
        && (keeping.checks_all() || crate::kept::compares(inputs))
    {
        let by_name: HashMap<&str, &Function> =
            all.iter().map(|function| (function.name.as_str(), *function)).collect();
        for (name, kept, keys, used_again) in &found {
            let Some(function) = by_name.get(name).filter(|_| *used_again) else { continue };
            let given = params.get(*name).map_or(&[][..], Vec::as_slice);
            let again = analysis_key(&analyze(function, changes, keeps, given, in_entries));
            // The analysis used again, and the key it carries, are each what analyzing again gives.
            if again != analysis_key(kept) || keys.is_none_or(|(_, key)| key != again) {
                differ.push((*name).to_owned());
            }
        }
        differ.sort();
    }
    let ir_keys = found.iter().filter_map(|(name, _, keys, _)| keys.map(|(ir, _)| (*name, ir))).collect();
    let analysis_keys =
        found.iter().filter_map(|(name, _, keys, _)| keys.map(|(_, key)| (*name, key))).collect();
    (
        found.into_iter().map(|(name, analysis, ..)| (name, analysis)).collect(),
        (ir_keys, analysis_keys),
        differ,
    )
}

/// An analysis's key, as a unit's read of it is compared (`Reads::value_of`): a kept analysis
/// carries it, so the read is a lookup.
pub(crate) fn analysis_key(analysis: &Analysis) -> Key {
    let mut state = crate::keys::Stable::new("fennec unit read 2026-11-17");
    analysis.stable(&mut state);
    state.key()
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
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[], &|_| false);
        let at = function.locals.iter().position(|local| local.name.as_deref() == Some("unit"));
        Ok(known.taken_over_loop(LocalId(at.ok_or("no `unit`")?)))
    }

    /// Whether the local called `name` in a function called `go` is made alone on the way into a
    /// loop, with `imports` before the types.
    fn alone_in_a_loop(imports: &str, body: &str, name: &str) -> Result<bool, String> {
        let source = format!("{imports}{TYPES}{body}");
        let mut module = fennec_syntax::parse(&source).map_err(|err| format!("{err:?}"))?;
        let checked =
            fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}\n{source}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
        let function = program
            .functions
            .iter()
            .find(|function| function.name == "go")
            .ok_or_else(|| format!("no `go` among {names:?}"))?;
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[], &|_| false);
        let at = function.locals.iter().position(|local| local.name.as_deref() == Some(name));
        let held = LocalId(at.ok_or_else(|| format!("no `{name}`"))?);
        Ok(known.alone_in_loops.iter().any(|alone| alone.locals.contains(&held)))
    }

    /// A list or a `Table` a loop changes is made alone on the way in, once, when nothing in the
    /// loop can give it another owner, however it's shared before or after. Copied in the loop,
    /// given to a call or a closure, put inside another value, written anew, held by a task, or
    /// held as it was by a loop through it from a place, it isn't: each change in the loop checks
    /// for itself.
    #[test]
    fn a_value_a_loop_gives_no_owner_is_made_alone_on_the_way_in() -> Result<(), String> {
        let summed = "def summed(items: list[int]) -> int:\n    return sum(items)\n\n";
        let around = |inside: &str| {
            format!(
                "{summed}def go(count: int) -> int:\n    mut items = list[int]()\n    mut seen = 0\n    mut lists = list[list[int]]()\n    for at in range(count):\n        items.append(at)\n{inside}    return seen + summed(items) + len(lists)\n"
            )
        };
        assert!(alone_in_a_loop("", &around(""), "items")?, "shared only after the loop");
        for (what, inside) in [
            ("copied", "        kept = items\n        seen += len(kept)\n"),
            ("given to a call", "        seen += summed(items)\n"),
            ("given to a closure", "        length = () => len(items)\n        seen += length()\n"),
            ("put inside another value", "        lists.append(items)\n"),
            ("written anew", "        if at == 3:\n            items = [at]\n"),
        ] {
            assert!(
                !alone_in_a_loop("", &around(inside), "items").map_err(|err| format!("{what}: {err}"))?,
                "{what}"
            );
        }
        // A loop through it from a place holds it as it was, so it's held twice on the way in:
        // the body's first append grows it into a block of its own, with no copy made first.
        let walked = "def go(count: int) -> int:\n    mut items = [1, 2, 3]\n    for item in items[1:]:\n        items.append(item + count)\n    return len(items)\n";
        assert!(!alone_in_a_loop("", walked, "items")?, "held as it was by a loop through it from a place");
        let tables = "from std.collections import Id, Table\nfrom std.concurrent import Crashed\n\n";
        let units = |inside: &str| {
            format!(
                "def report(units: Table[Unit]) -> int:\n    return len(units)\n\ndef go(ref units: Table[Unit], ids: list[Id[Unit]]) fails Crashed:\n    for id in ids:\n        units[id].hp += 1\n{inside}    return\n"
            )
        };
        assert!(
            alone_in_a_loop(tables, &units(""), "units").map_err(|err| format!("a `Table`: {err}"))?,
            "a `Table` changed by `Id`"
        );
        let task = "        with launch(() => report(units))?:\n            pass\n";
        assert!(
            !alone_in_a_loop(tables, &units(task), "units").map_err(|err| format!("a task: {err}"))?,
            "held by a task"
        );
        Ok(())
    }

    /// An item or a place is noted as taken out only for a local with one write, whatever the
    /// finding that took it checked: what the emitter knows of it is noted as it writes that
    /// write, so a second write's reads, written before it or after, would be named wrongly.
    #[test]
    fn only_a_local_written_once_is_taken_out() {
        let mut known = Analysis::empty(3);
        known.writes = vec![0, 1, 2];
        for at in 0..3 {
            let local = LocalId(at);
            let item = known.take_item(local, (LocalId(0), vec!["units".to_owned()]));
            let place = known.take_place(local, 0);
            let once = at == 1;
            assert_eq!((item, place), (once, once), "a local written {at} times");
            assert_eq!(known.item_home(local).is_some(), once, "a local written {at} times");
            assert_eq!(known.places_taken.contains_key(&at), once, "a local written {at} times");
            assert_eq!(known.is_taken(local), once, "a local written {at} times");
        }
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

    /// Whether the list a function called `go` appends to, through a field of a dict's value or a
    /// list's item, is taken out for it, whether it's taken only when the append comes, and
    /// whether each item read again to be put back is taken too, from where it goes back.
    fn field_taken(body: &str) -> Result<(bool, bool, bool), String> {
        let types = "struct Bag:\n    items: list[str]\n    count: int\n\nstruct Shelf:\n    named: dict[str, Bag]\n    rows: list[Bag]\n    total: int\n\ndef made(turn: int) -> str:\n    return f\"made in turn {turn}\"\n\ndef counted(ref shelf: Shelf) -> str:\n    shelf.total += 1\n    return \"counted\"\n\n";
        let source = format!("{types}{body}");
        let mut module = fennec_syntax::parse(&source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let changes = |name: &str| if name == "counted" { vec![0] } else { Vec::new() };
        let known = analyze(function, &changes, &|_, _| false, &[], &|_| false);
        let appended =
            function.blocks.iter().flat_map(|block| &block.insts).find_map(|inst| match &inst.kind {
                InstKind::Builtin { name, args, .. } if name == "list.append" => match args.first() {
                    Some(Operand::Local(list)) => Some(*list),
                    _ => None,
                },
                _ => None,
            });
        let appended = appended.ok_or("no append")?;
        // Every item put back whole is one read again and taken, with the put-back's place as its
        // home: one read and kept would hold the list's old address once the list grows.
        let put_backs: Vec<bool> = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter_map(|inst| match &inst.kind {
                InstKind::SetIndex { object, path, value: Operand::Local(item), .. } => {
                    Some(known.items_taken.get(&item.0) == Some(&(*object, path.clone())))
                }
                _ => None,
            })
            .collect();
        let put_back_taken = !put_backs.is_empty() && put_backs.iter().all(|taken| *taken);
        Ok((known.path_home(appended).is_some(), known.path_waits(appended), put_back_taken))
    }

    /// `shelf.named[key].items.append(x)` takes the list out, not a copy of it, whatever `x` is.
    /// When `x` is a local, the list is taken where it's read. When `x` is made on the way, a
    /// string, a call's result, something read from the shelf, the list is taken only when the
    /// append comes, so nothing is out while `x` is made: before this, each of those appended to
    /// a copy, which copied the whole list every time.
    #[test]
    fn a_field_through_a_dict_is_taken_whatever_is_appended() -> Result<(), String> {
        let go = "def go(ref shelf: Shelf, key: str, text: str, turn: int):\n    ";
        for (appended, waits) in [
            ("text", false),
            ("f\"made in turn {turn}\"", true),
            ("made(turn)", true),
            ("str(shelf.total)", true),
            ("made(shelf.named[key].count)", true),
        ] {
            let body = format!("{go}shelf.named[key].items.append({appended})\n");
            let (taken, waited, _) = field_taken(&body)?;
            assert_eq!((taken, waited), (true, waits), "{body}");
        }
        Ok(())
    }

    /// Written straight in a lock's body, `held.named[key].items.append(x)` takes the list out
    /// through the lock's handle, two fields up from the dict, whatever `x` is: the IR reads
    /// `held.value` into a new local on the way out and again on the way back, and both are the
    /// same place from the handle. The bag read on the way back is taken too, with the same home.
    /// A list's item, for `held.rows[i].items.append(x)`, is taken out of the list whole, so its
    /// list is its own and grows where it is, with no path taken. Before, the take looked one
    /// field up, from a different local each way, found nothing, and every append copied the
    /// whole list.
    #[test]
    fn a_field_in_a_locks_own_body_is_taken_through_its_handle() -> Result<(), String> {
        let go = "def go(shelf: shared Shelf, key: str, text: str, turn: int):\n    lock shelf as ref held:\n        ";
        for (appended, waits) in [("text", false), ("f\"made in turn {turn}\"", true), ("made(turn)", true)] {
            let body = format!("{go}held.named[key].items.append({appended})\n");
            assert_eq!(field_taken(&body)?, (true, waits, true), "{body}");
            let body = format!("{go}held.rows[turn].items.append({appended})\n");
            assert_eq!(field_taken(&body)?, (false, false, true), "{body}");
        }
        Ok(())
    }

    /// Each function's analysis is kept from one build to the next and used again when its
    /// function and what it asked of the program are as they were, and only then: after a body's
    /// edit, every other function's is used again, and after a callee stops changing what it's
    /// handed by `ref`, its caller's isn't. Every analysis a build uses, kept or not, is what
    /// analyzing its function afresh gives.
    #[test]
    fn an_analysis_is_used_again_only_when_its_function_and_what_it_asked_are_as_they_were()
    -> Result<(), String> {
        let source = |grow: &str, scale: &str| {
            format!(
                "def grow({grow}) -> int:\n    return len(items)\n\ndef caller() -> int:\n    mut items = [1, 2, 3]\n    return grow({})\n\ndef other(count: int) -> int:\n    scale = {scale}\n    return count * scale\n\ndef main(stdout: Stdout):\n    stdout.print(caller(), other(4))\n    return\n",
                if grow.starts_with("ref") { "ref items" } else { "items" }
            )
        };
        let lowered = |text: &str| -> Result<Program, String> {
            let mut module = fennec_syntax::parse(text).map_err(|err| format!("{err:?}"))?;
            let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
            Ok(fennec_ir::lower(&module, &checked))
        };
        // What a build of `program` uses, starting from `kept`: its analyses' keys, how many were
        // used again, and what it keeps.
        let built = |program: &Program, kept: Option<crate::kept::KeptAnalyses>| {
            let keeping = crate::kept::Keeping::from(kept);
            let (whole, differ) = whole_keeping(program, &keeping);
            let keys: BTreeMap<String, Key> = whole
                .analyses
                .iter()
                .map(|(name, analysis)| ((*name).to_owned(), analysis_key(analysis)))
                .collect();
            (keys, differ, keeping.used_again(), keeping.into_kept())
        };
        let afresh = |program: &Program| -> BTreeMap<String, Key> {
            whole_keeping(program, &crate::kept::Keeping::from(None))
                .0
                .analyses
                .iter()
                .map(|(name, analysis)| ((*name).to_owned(), analysis_key(analysis)))
                .collect()
        };
        let first = lowered(&source("ref items: list[int]", "3"))?;
        let (keys, _, used, kept) = built(&first, None);
        assert_eq!((used, keys.len()), (0, first.functions.len() + first.constants.len()));
        assert_eq!(keys, afresh(&first));
        // A body's edit: `other` is analyzed again, and every other function's is used again.
        let edited = lowered(&source("ref items: list[int]", "4"))?;
        let (keys, differ, used, kept) = built(&edited, Some(kept));
        assert!(differ.is_empty(), "{differ:?}");
        assert_eq!(used, keys.len() - 1, "a body's edit used again {used} of {}", keys.len());
        assert_eq!(keys, afresh(&edited));
        // `grow` no longer changes what it's handed: `caller`, which asked, is analyzed again too.
        let plain = lowered(&source("items: list[int]", "4"))?;
        let (keys, _, used, _) = built(&plain, Some(kept));
        assert!(used <= keys.len() - 2, "a callee's change used again {used} of {}", keys.len());
        assert_eq!(keys, afresh(&plain));
        // What a function asks changes while its IR doesn't: `other`'s parameter can be negative
        // now, and `kept` stops returning what it's handed, which `holder` asked about. Neither's
        // analysis is the one kept, as using it would show.
        let asked = |count: &str, kept: &str| {
            format!(
                "def kept(items: list[int]) -> list[int]:\n    return {kept}\n\ndef holder() -> int:\n    items = [1, 2, 3]\n    held = kept(items)\n    return len(held) + items[0]\n\ndef other(count: int) -> int:\n    return [5, 6, 7, 8, 9][count]\n\ndef main(stdout: Stdout):\n    stdout.print(holder(), other({count}))\n    return\n"
            )
        };
        let mut kept = None;
        for (count, returned) in [("2", "items"), ("-2", "items"), ("-2", "[4, 5]")] {
            let program = lowered(&asked(count, returned))?;
            let (keys, differ, _, now) = built(&program, kept.take());
            assert!(differ.is_empty(), "{differ:?}");
            assert_eq!(keys, afresh(&program), "other({count}), kept returning {returned}");
            kept = Some(now);
        }
        Ok(())
    }

    /// A build keeps the build before's analyses of functions it doesn't have for one build more,
    /// as `fennec run` building the tests and then the program needs, and lets older ones go.
    #[test]
    fn the_build_befores_analyses_are_kept_one_build_more_and_no_longer() -> Result<(), String> {
        // `main` and `shared_part` are the same in every program; each has one function of its own.
        let source = |own: &str| {
            format!(
                "def shared_part(count: int) -> int:\n    return count * 2\n\ndef {own}() -> int:\n    return shared_part(3)\n\ndef main(stdout: Stdout):\n    stdout.print(shared_part(4))\n    return\n"
            )
        };
        let lowered = |text: &str| -> Result<Program, String> {
            let mut module = fennec_syntax::parse(text).map_err(|err| format!("{err:?}"))?;
            let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
            Ok(fennec_ir::lower(&module, &checked))
        };
        // How many analyses a build of `program` used again, starting from `kept`, and what it keeps.
        let built = |program: &Program, kept: Option<crate::kept::KeptAnalyses>| {
            let keeping = crate::kept::Keeping::from(kept);
            let (whole, differ) = whole_keeping(program, &keeping);
            assert!(differ.is_empty(), "{differ:?}");
            (keeping.used_again(), whole.analyses.len(), keeping.into_kept())
        };
        let (tests, program, other) = (
            lowered(&source("for_tests"))?,
            lowered(&source("for_program"))?,
            lowered(&source("elsewhere"))?,
        );
        let (_, _, kept) = built(&tests, None);
        // The program, after the tests: all but its own function used again.
        let (used, of, kept) = built(&program, Some(kept));
        assert_eq!(used, of - 1, "the program after the tests");
        // The tests again: every analysis used again, `for_tests`'s from two builds back.
        let (used, of, kept) = built(&tests, Some(kept));
        assert_eq!(used, of, "the tests again");
        // Another program, then the program: `for_program`'s, three builds back, is gone.
        let (_, _, kept) = built(&other, Some(kept));
        let (used, of, _) = built(&program, Some(kept));
        assert_eq!(used, of - 1, "the program after another");
        Ok(())
    }

    /// Whether the one list a function called `go` fills is filled with no room check: the
    /// list its comprehension makes, before it's given a name.
    fn within_room(body: &str) -> Result<bool, String> {
        let module = fennec_syntax::parse(body).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[], &|_| false);
        Ok(!known.room_enough.is_empty())
    }

    /// A comprehension over a count from 0 or more fills its list with no room check: room
    /// for every turn is made first, and nothing else touches the list while it's filled. So
    /// does a walk through a `Table`'s live slots, and a named list, filled by one loop. One
    /// counting from below 0 or from a variable could outgrow its room, and one appending in a
    /// loop inside the loop could append more than once a turn, so those keep the check. A
    /// filter may append on few turns, so no room is made for every one. What's known of a list
    /// while its loop fills it goes by its local, so a list appended to outside the loop, after
    /// it or in a second loop, keeps the check everywhere.
    #[test]
    fn only_a_list_filled_within_its_room_skips_the_check() -> Result<(), String> {
        let filled = |body: &str| format!("def go(n: int) -> int:\n{body}    return len(xs)\n");
        for body in [
            "    xs = [i * 2 for i in range(n)]\n",
            "    xs = [i for i in range(1, n)]\n",
            "    mut xs = list[int]()\n    for i in range(n):\n        xs.append(i)\n",
        ] {
            assert!(within_room(&filled(body))?, "kept the check: {body}");
        }
        let walked = "from std.collections import Table\n\nstruct Unit:\n    hp: int\n\ndef go(units: Table[Unit]) -> int:\n    hps = [unit.hp for unit in units]\n    return len(hps)\n";
        assert!(within_room(walked)?, "kept the check: a walk through a Table");
        for body in [
            "    xs = [i for i in range(-3, n)]\n",
            "    start = n - 5\n    xs = [i for i in range(start, n)]\n",
            "    mut xs = list[int]()\n    for i in range(n):\n        for j in range(i):\n            xs.append(j)\n",
            "    xs = [i for i in range(n) if i % 2 == 0]\n",
            "    mut xs = list[int]()\n    for i in range(n):\n        if i > 3:\n            continue\n        xs.append(i)\n",
            "    mut xs = list[int]()\n    for i in range(n):\n        xs.append(i)\n    xs.append(n)\n",
            "    mut xs = list[int]()\n    for i in range(n):\n        xs.append(i)\n    for i in range(n):\n        xs.append(i)\n",
            "    mut xs = [n]\n    xs.append(n)\n    m = len(xs)\n    xs = []\n    for i in range(m):\n        xs.append(i)\n",
        ] {
            assert!(!within_room(&filled(body))?, "skipped the check: {body}");
        }
        let filtered = walked.replace("for unit in units]", "for unit in units if unit.hp > 0]");
        assert!(!within_room(&filtered)?, "skipped the check: a walk through a Table with a filter");
        Ok(())
    }

    /// The named list locals of a function called `go` whose length is kept (`steady_lengths`),
    /// with each call changing what its `ref` parameters are given, as a build has it: `grow`, and
    /// std's methods taking `ref self`, like `extend`.
    fn steady(body: &str) -> Result<Vec<String>, String> {
        let source = format!(
            "def grow(ref xs: list[int]):\n    xs.append(1)\n    return\n\ndef total(xs: list[int]) -> int:\n    return sum(xs)\n\n{body}"
        );
        let mut module = fennec_syntax::parse(&source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let changes = |name: &str| {
            program
                .functions
                .iter()
                .find(|callee| callee.name == name)
                .map_or_else(Vec::new, |callee| callee.mut_params.clone())
        };
        let known = analyze(function, &changes, &|_, _| false, &[], &|_| false);
        let mut names: Vec<String> = known
            .steady_lengths
            .iter()
            .filter_map(|at| function.locals.get(*at).and_then(|local| local.name.clone()))
            .collect();
        names.sort();
        Ok(names)
    }

    /// A list parameter only read and stored into in a loop keeps its length, a plain one or a
    /// `ref` one, so a check against it can leave the loop. One appended to, popped, handed by
    /// `ref` to a call that changes it, or given another value, doesn't, nor one read outside any
    /// loop, which gains nothing, nor a list made in the function.
    #[test]
    fn a_list_only_read_and_stored_into_keeps_its_length() -> Result<(), String> {
        let kept = "def go(xs: list[int], ref ys: list[int]) -> int:\n    zs = [1, 2, 3]\n    mut total = 0\n    for at in range(len(xs)):\n        ys[at % len(ys)] = xs[at] + zs[at % 3]\n        total += xs[at]\n    return total\n";
        // `zs` is made here, with its length written where LLVM sees it, so it's left alone.
        assert_eq!(steady(kept)?, ["xs", "ys"]);
        // Each change in the loop that reads the list, so only the change can be what leaves it out.
        let looped = |change: &str| {
            format!(
                "def go(ref xs: list[int]) -> int:\n    mut got = 0\n    for at in range(3):\n        {change}\n        got += xs[at]\n    return got\n"
            )
        };
        for (change, left_out) in [
            ("xs.append(1)", "appended to"),
            ("_ = xs.pop()", "popped"),
            ("xs.extend([1, 2])", "extended"),
            ("xs.swap(0, 1)", "swapped, which a builtin that changes the list does"),
            ("grow(ref xs)", "handed by ref to a call that changes it"),
            ("xs = [4, 5, 6]", "given a new value"),
        ] {
            let body = looped(change);
            assert_eq!(steady(&body)?, Vec::<String>::new(), "kept though {left_out}: {body}");
        }
        // Handed to a call by value, a list keeps its length: the call has its own copy.
        let handed = "def go(xs: list[int]) -> int:\n    mut got = 0\n    for at in range(3):\n        got += total(xs) + xs[at]\n    return got\n";
        assert_eq!(steady(handed)?, ["xs"]);
        // Read outside any loop, its length is read once anyway, so nothing is kept.
        let once = "def go(xs: list[int]) -> int:\n    return xs[0] + len(xs)\n";
        assert_eq!(steady(once)?, Vec::<String>::new());
        Ok(())
    }

    /// How many reads of a function called `go` are paired with the read of the next index.
    fn adjacent(body: &str) -> Result<usize, String> {
        let mut module = fennec_syntax::parse(body).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let function = program.functions.iter().find(|function| function.name == "go").ok_or("no `go`")?;
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[], &|_| false);
        Ok(known.adjacent_reads.len())
    }

    /// `xs[i]` then `xs[i + 1]` are paired, so one test can put both inside; not when something
    /// that could change the list or `i` comes between, when the second index isn't `i + 1`, or
    /// when `xs[i + 1]` is read first.
    #[test]
    fn a_read_and_the_read_of_the_next_index_are_paired() -> Result<(), String> {
        let paired = "def go(xs: list[int], i: int) -> int:\n    return xs[i] * 10 + xs[i + 1]\n";
        assert_eq!(adjacent(paired)?, 2);
        for body in [
            "def go(ref xs: list[int], i: int) -> int:\n    a = xs[i]\n    xs.append(a)\n    return a + xs[i + 1]\n",
            "def go(xs: list[int], i: int) -> int:\n    return xs[i] * 10 + xs[i + 2]\n",
            "def go(xs: list[int], ys: list[int], i: int) -> int:\n    return xs[i] * 10 + ys[i + 1]\n",
            // `i` given a new value between the two reads, by a read.
            "def go(xs: list[int], start: int) -> int:\n    mut i = start\n    a = xs[i]\n    i = xs[0]\n    return a + xs[i + 1]\n",
            // The list given a new value between the two reads.
            "def go(xs: list[int], ys: list[int], i: int) -> int:\n    mut zs = xs\n    a = zs[i]\n    zs = ys\n    return a + zs[i + 1]\n",
            // The next index read first isn't paired (yet).
            "def go(xs: list[int], i: int) -> int:\n    return xs[i + 1] - xs[i]\n",
        ] {
            assert_eq!(adjacent(body)?, 0, "{body}");
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
        let known = analyze(function, &|_| Vec::new(), &|_, _| false, &[], &|_| false);
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

// A function's analysis as bytes, kept by the daemon between builds and handed to the process that
// builds (see `crate::kept`). Every field is named, so one added later has to be written too.
fennec_syntax::codec_fields!(Analysis {
    safe,
    nonzero,
    below,
    length_of,
    reads,
    writes,
    shared,
    changed,
    maybe_marked,
    given_mut,
    read_again,
    borrowed,
    borrowed_from,
    taken,
    items_taken,
    put_back_reads,
    slot_homes,
    path_taken,
    path_waits,
    path_waiting,
    loop_taken,
    places_taken,
    place_parts,
    place_paths,
    places_waiting,
    field_of,
    slice_keys,
    unmade,
    aliases,
    alone_in_loops,
    steady_lengths,
    adjacent_reads,
    reservations,
    zero_filled,
    zero_counts,
    room_enough,
    length_kept,
    last_copies,
    last_reads,
    last_made,
    enumerated,
    map_walks,
    walk_items,
    walk_keys,
    pairs,
    guarded_reads,
    ranged_reads,
    range_scans,
    versioned,
    param_ranges,
    slice_ranges,
});
fennec_syntax::codec_fields!(Versioned { header, before, blocks, scan });
fennec_syntax::codec_fields!(RangeScan { bounds, read, before, count, stop, early });
fennec_syntax::codec_fields!(AloneInLoop { entry, blocks, locals });
fennec_syntax::codec_fields!(Reserved { list, limit, copied_from });
