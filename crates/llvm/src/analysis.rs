//! What's known about each function before emitting it: which of its checks can't fail, from
//! `fennec_ir::facts`, which lists keep one length, who a value can move to, and so which
//! functions can fail at all.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_ir::facts::{Facts, Range};
use fennec_ir::{Function, GeneratorDef, InstKind, LocalId, Operand, Program, Terminator};
use fennec_syntax::{BinOp, UnaryOp};

const FULL: Range = (i64::MIN, i64::MAX);
/// The most items anything in memory can have.
pub(crate) const MOST_ITEMS: i64 = 1 << 48;

/// What's known about one function.
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
    /// Locals standing for a list that a `for` loop goes through while changing only the item
    /// it's on: a copy of the list would say the same, and cost a whole list.
    aliases: HashMap<usize, LocalId>,
    /// A block ending in a jump to a loop that fills a list made in that block: the list, and
    /// how many items the loop adds at most.
    pub(crate) reservations: HashMap<usize, (usize, Operand)>,
}

/// What's known about every function, and which can fail.
pub(crate) struct Whole<'p> {
    pub(crate) functions: HashMap<&'p str, &'p Function>,
    pub(crate) analyses: HashMap<&'p str, Analysis>,
    pub(crate) can_fail: HashSet<&'p str>,
    /// Each generator, by its name and by the names of its functions.
    pub(crate) generators: HashMap<&'p str, &'p GeneratorDef>,
}

/// What a function calls by name, a constant being a call of a function of no arguments.
pub(crate) fn callees(function: &Function) -> impl Iterator<Item = &str> {
    function.blocks.iter().flat_map(|block| &block.insts).filter_map(|inst| match &inst.kind {
        InstKind::Call { function, .. } => Some(function.as_str()),
        InstKind::Constant { name, .. } => Some(name.as_str()),
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
    changes: &dyn Fn(&str) -> Vec<usize>,
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
                        }
                    }
                    for arg in args {
                        if let Operand::Local(local) = arg {
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

fn analyze(function: &Function, changes: &dyn Fn(&str) -> Vec<usize>) -> Analysis {
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
        read_again: HashSet::new(),
        borrowed: HashSet::new(),
        borrowed_from: HashMap::new(),
        taken: HashSet::new(),
        items_taken: HashMap::new(),
        aliases: HashMap::new(),
        reservations: HashMap::new(),
    };
    let grown = count_uses(function, changes, &mut known);
    find_reads_again(function, &mut known);
    // A copy shares its source, unless the source is never read again, and a block put inside
    // something else is shared by that.
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        match &inst.kind {
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
    find_borrows(function, &mut known);
    find_aliases(function, &mut known);
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
    known
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
fn find_borrows(function: &Function, known: &mut Analysis) {
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
                for next in later {
                    if only_looks(&next.kind, dest) {
                        known.borrowed.insert(dest.0);
                        known.borrowed_from.insert(dest.0, source);
                        break;
                    }
                    if !is_quiet(&next.kind) {
                        break;
                    }
                }
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
                    InstKind::SetIndex { object, path, .. } if *object == dest && path.is_empty() => {}
                    InstKind::Make { .. } | InstKind::Copy { .. } => {}
                    other if is_quiet(other) => {}
                    _ => break,
                }
            }
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
        }
    }
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
    pub(crate) fn is_taken(&self, local: LocalId) -> bool {
        self.taken.contains(&local.0)
    }

    /// The list a local stands for, when it needs no copy of its own.
    pub(crate) fn alias(&self, local: LocalId) -> Option<LocalId> {
        self.aliases.get(&local.0).copied()
    }

    /// Where the list is that a local took an item out of.
    pub(crate) fn item_home(&self, local: LocalId) -> Option<&(LocalId, Vec<String>)> {
        self.items_taken.get(&local.0)
    }

    /// Whether anything writes a local.
    pub(crate) fn written(&self, local: LocalId) -> bool {
        self.writes.get(local.0).is_some_and(|writes| *writes > 0)
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
            (Some(Type::Int), BinOp::Pow) => true,
            _ => false,
        },
        InstKind::Unary { dest, op: UnaryOp::Neg, value } => {
            function.type_of(value) == Some(Type::Int) && !known.is_safe(*dest)
        }
        InstKind::Builtin { name, .. } => {
            matches!(
                name.as_str(),
                "math.sqrt"
                    | "str.code"
                    | "int.abs"
                    | "sum"
                    | "sum.start"
                    | "list.pop"
                    | "sorted.key"
                    | "list.remove_where"
                    | "result.expect"
                    | "Regex.replace"
                    | "Regex.split"
                    | "Regex.find_all"
                    | "Regex.search"
                    | "Regex.match"
                    | "Regex.fullmatch"
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
        | InstKind::Constant { .. } => true,
        _ => false,
    };
    function.blocks.iter().any(|block| {
        matches!(block.end, Terminator::Stop { .. } | Terminator::NoMatch { .. })
            || block.insts.iter().any(|inst| inst_fails(&inst.kind))
    })
}

pub(crate) fn whole(program: &Program) -> Whole<'_> {
    let every = || program.functions.iter().chain(&program.constants);
    let functions: HashMap<&str, &Function> =
        every().map(|function| (function.name.as_str(), function)).collect();
    let changes = |callee: &str| functions.get(callee).map_or_else(Vec::new, |f| f.mut_params.clone());
    let analyses: HashMap<&str, Analysis> =
        every().map(|function| (function.name.as_str(), analyze(function, &changes))).collect();
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
    Whole { functions, analyses, can_fail, generators }
}
