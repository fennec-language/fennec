//! What's known about each function before emitting it: which of its checks can't fail, from
//! `fennec_ir::facts`, which lists keep one length, who a value can move to, and so which
//! functions can fail at all.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_ir::facts::{Facts, Range};
use fennec_ir::{Function, InstKind, LocalId, Operand, Program, Terminator};
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
    /// A block ending in a jump to a loop that fills a list made in that block: the list, and
    /// how many items the loop adds at most.
    pub(crate) reservations: HashMap<usize, (usize, Operand)>,
}

/// What's known about every function, and which can fail.
pub(crate) struct Whole<'p> {
    pub(crate) functions: HashMap<&'p str, &'p Function>,
    pub(crate) analyses: HashMap<&'p str, Analysis>,
    pub(crate) can_fail: HashSet<&'p str>,
}

pub(crate) fn callees(function: &Function) -> impl Iterator<Item = &str> {
    function.blocks.iter().flat_map(|block| &block.insts).filter_map(|inst| match &inst.kind {
        InstKind::Call { function, .. } => Some(function.as_str()),
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
fn visit(inst: &InstKind, mut read: impl FnMut(&Operand), mut write: impl FnMut(LocalId)) {
    let mut local = |id: LocalId| read(&Operand::Local(id));
    match inst {
        InstKind::SetField { object, value, .. } => {
            local(*object);
            read(value);
        }
        InstKind::SetIndex { object, index, value, .. } => {
            local(*object);
            read(index);
            read(value);
        }
        InstKind::Copy { dest, value }
        | InstKind::Unary { dest, value, .. }
        | InstKind::GetField { dest, object: value, .. }
        | InstKind::GetTupleItem { dest, value, .. } => {
            read(value);
            write(*dest);
        }
        InstKind::Binary { dest, lhs, rhs, .. } => {
            read(lhs);
            read(rhs);
            write(*dest);
        }
        InstKind::Make { dest, fields, .. } => {
            fields.iter().for_each(&mut read);
            write(*dest);
        }
        InstKind::MakeList { dest, items } | InstKind::MakeTuple { dest, items } => {
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
        reservations: HashMap::new(),
    };
    let grown = count_uses(function, changes, &mut known);
    // A copy shares its source, unless the source is never read again.
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        if let InstKind::Copy { value: Operand::Local(from), .. } = &inst.kind
            && !known.moves(*from)
        {
            known.shared.insert(from.0);
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
    known
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
        self.reads.get(from.0) == Some(&1) && self.writes.get(from.0) == Some(&1)
    }

    /// Whether this function changes a list in a local nothing else here comes to hold, so
    /// it's enough to see that it has one owner when the local gets it.
    pub(crate) fn alone_once_checked(&self, local: LocalId) -> bool {
        self.changed.contains(&local.0) && !self.shared.contains(&local.0)
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
            (Some(Type::Int), BinOp::FloorDiv | BinOp::Mod) => !matches!(rhs, Operand::Int(1..)),
            (Some(Type::Float), BinOp::Div) => !known.is_nonzero(rhs),
            _ => false,
        },
        InstKind::Unary { dest, op: UnaryOp::Neg, value } => {
            function.type_of(value) == Some(Type::Int) && !known.is_safe(*dest)
        }
        InstKind::Builtin { name, .. } => name == "math.sqrt",
        InstKind::GetIndex { object: Operand::Local(list), index, .. } => !known.inside(*list, index),
        InstKind::SetIndex { object, index, .. } => !known.inside(*object, index),
        InstKind::GetIndex { .. } | InstKind::Call { .. } | InstKind::Format { .. } => true,
        _ => false,
    };
    function.blocks.iter().any(|block| {
        matches!(block.end, Terminator::Stop { .. } | Terminator::NoMatch { .. })
            || block.insts.iter().any(|inst| inst_fails(&inst.kind))
    })
}

pub(crate) fn whole(program: &Program) -> Whole<'_> {
    let functions: HashMap<&str, &Function> =
        program.functions.iter().map(|function| (function.name.as_str(), function)).collect();
    let changes = |callee: &str| functions.get(callee).map_or_else(Vec::new, |f| f.mut_params.clone());
    let analyses: HashMap<&str, Analysis> = program
        .functions
        .iter()
        .map(|function| (function.name.as_str(), analyze(function, &changes)))
        .collect();
    let can_fail = program
        .functions
        .iter()
        .filter(|function| {
            analyses.get(function.name.as_str()).is_none_or(|known| fails_itself(function, known))
        })
        .map(|function| function.name.as_str())
        .collect();
    Whole { functions, analyses, can_fail }
}
