//! What's known about a function's locals before it's emitted: the same facts the JIT's
//! `ranges.rs` works out from the syntax tree, here from the IR. Int ranges come from constants,
//! lengths and `for` loops. A sum whose range fits can't overflow, an index a loop keeps below a
//! list's length can't be out of range, and a comprehension's list knows how big it gets.

use std::collections::{HashMap, HashSet};

use fennec_ir::{BlockId, Function, InstKind, LocalId, Operand, Terminator};
use fennec_syntax::BinOp;

const MOST_ITEMS: i128 = 1 << 48;
const FULL: (i128, i128) = (i64::MIN as i128, i64::MAX as i128);

#[derive(Default)]
pub struct Facts {
    ranges: Vec<(i128, i128)>,
    /// A loop's copy of its count, and the limit it stays below.
    below: HashMap<usize, Operand>,
    /// A local holding the length of a list that can't grow, and which list.
    length_of: Vec<Option<usize>>,
    /// Lists that keep one length: assigned once, never appended to or changed by a call.
    fixed_lists: HashSet<usize>,
    nonzero: HashSet<usize>,
    /// The locals an add, subtract or multiply makes that can't overflow.
    pub safe: HashSet<usize>,
    /// Locals to tell LLVM the range of, since inlining can use it where this can't.
    pub assumes: HashMap<usize, (i64, i64)>,
    /// A block before a loop filling a list: the list, and how many times the loop runs.
    pub reserve: HashMap<usize, (usize, Operand)>,
}

fn dest(inst: &InstKind) -> Option<LocalId> {
    match inst {
        InstKind::Copy { dest, .. }
        | InstKind::Unary { dest, .. }
        | InstKind::Binary { dest, .. }
        | InstKind::Make { dest, .. }
        | InstKind::GetField { dest, .. }
        | InstKind::MakeCase { dest, .. }
        | InstKind::IsCase { dest, .. }
        | InstKind::GetCaseField { dest, .. }
        | InstKind::MakeList { dest, .. }
        | InstKind::MakeSet { dest, .. }
        | InstKind::MakeDict { dest, .. }
        | InstKind::MakeTuple { dest, .. }
        | InstKind::GetTupleItem { dest, .. }
        | InstKind::GetIndex { dest, .. }
        | InstKind::Slice { dest, .. }
        | InstKind::Format { dest, .. }
        | InstKind::MakeSome { dest, .. }
        | InstKind::IsNone { dest, .. }
        | InstKind::GetSome { dest, .. }
        | InstKind::MakeOk { dest, .. }
        | InstKind::MakeErr { dest, .. }
        | InstKind::IsErr { dest, .. }
        | InstKind::GetOk { dest, .. }
        | InstKind::GetErr { dest, .. }
        | InstKind::IsError { dest, .. }
        | InstKind::AsError { dest, .. }
        | InstKind::MakeClosure { dest, .. }
        | InstKind::Constant { dest, .. } => Some(*dest),
        InstKind::Builtin { dest, .. } | InstKind::CallClosure { dest, .. } | InstKind::Call { dest, .. } => *dest,
        InstKind::SetField { .. } | InstKind::SetIndex { .. } => None,
    }
}

impl Facts {
    fn range(&self, operand: &Operand) -> (i128, i128) {
        match operand {
            Operand::Local(id) => self.ranges.get(id.0).copied().unwrap_or(FULL),
            Operand::Int(value) => (i128::from(*value), i128::from(*value)),
            _ => FULL,
        }
    }

    /// Whether `list[index]` can't be out of range.
    pub fn inside(&self, list: &Operand, index: &Operand) -> bool {
        let (Operand::Local(list), Operand::Local(index)) = (list, index) else { return false };
        let Some(Operand::Local(limit)) = self.below.get(&index.0) else { return false };
        self.range(&Operand::Local(*index)).0 >= 0
            && self.fixed_lists.contains(&list.0)
            && self.length_of.get(limit.0).copied().flatten() == Some(list.0)
    }

    pub fn nonzero(&self, operand: &Operand) -> bool {
        match operand {
            Operand::Local(id) => self.nonzero.contains(&id.0),
            Operand::Float(value) => *value != 0.0,
            _ => false,
        }
    }

    /// `mut_param(callee, at)` says whether a call changes the local passed there.
    pub fn of(f: &Function, mut_param: &dyn Fn(&str, usize) -> bool) -> Facts {
        let count = f.locals.len();
        let mut facts = Facts {
            ranges: vec![FULL; count],
            length_of: vec![None; count],
            ..Facts::default()
        };
        let mut defs = vec![0_u32; count];
        for at in 0..f.params {
            defs[at] += 1;
        }
        let mut grows = HashSet::new();
        let insts = || f.blocks.iter().flat_map(|block| &block.insts).map(|inst| &inst.kind);
        for inst in insts() {
            if let Some(dest) = dest(inst) {
                defs[dest.0] += 1;
            }
            match inst {
                InstKind::Builtin { name, args, changes, .. } => {
                    if let (true, Some(Operand::Local(list))) = (*changes || name == "list.append", args.first()) {
                        grows.insert(list.0);
                    }
                }
                InstKind::Call { function, args, .. } => {
                    for (at, arg) in args.iter().enumerate() {
                        if let (true, Operand::Local(list)) = (mut_param(function, at), arg) {
                            grows.insert(list.0);
                        }
                    }
                }
                _ => {}
            }
        }
        let single = |id: LocalId| defs.get(id.0) == Some(&1);
        facts.fixed_lists = (0..count).filter(|at| defs[*at] == 1 && !grows.contains(at)).collect();

        // A count is an unnamed local set once and stepped by one, where the loop's test reads it.
        let mut starts: HashMap<usize, Operand> = HashMap::new();
        let mut stepped = HashSet::new();
        for inst in insts() {
            match inst {
                InstKind::Copy { dest, value } => {
                    starts.insert(dest.0, value.clone());
                }
                InstKind::Binary { dest, op: BinOp::Add, lhs, rhs: Operand::Int(1) } if *lhs == Operand::Local(*dest) => {
                    stepped.insert(dest.0);
                }
                _ => {}
            }
        }
        let is_count = |at: usize| defs[at] == 2 && stepped.contains(&at) && f.locals[at].name.is_none();
        let mut limits: HashMap<usize, Operand> = HashMap::new();
        let mut copies: HashMap<usize, (usize, Operand)> = HashMap::new();
        for block in &f.blocks {
            let Terminator::Branch { cond: Operand::Local(cond), then, .. } = &block.end else { continue };
            for inst in &block.insts {
                let InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(count_at), rhs } = &inst.kind else {
                    continue;
                };
                if dest != cond || !is_count(count_at.0) {
                    continue;
                }
                let steady = match rhs {
                    Operand::Local(limit) => single(*limit),
                    Operand::Int(_) => true,
                    _ => false,
                };
                if !steady {
                    continue;
                }
                limits.insert(count_at.0, rhs.clone());
                let Some(body) = f.blocks.get(then.0) else { continue };
                for inst in &body.insts {
                    if let InstKind::Copy { dest, value: Operand::Local(from) } = &inst.kind {
                        if from == count_at && single(*dest) {
                            copies.insert(dest.0, (count_at.0, rhs.clone()));
                        }
                    }
                }
            }
        }
        for (copy, (_, limit)) in &copies {
            facts.below.insert(*copy, limit.clone());
        }

        // Each pass only ever works from what's already sound, so stopping early is safe too.
        for _ in 0..4 {
            for (&count_at, limit) in &limits {
                let Some(start) = starts.get(&count_at) else { continue };
                let (low, high) = (facts.range(start), facts.range(limit));
                facts.ranges[count_at] = (low.0, high.1.max(low.1));
            }
            for inst in insts() {
                facts.step(inst, &single, &copies);
            }
        }
        for (&copy, _) in &copies {
            let (low, high) = facts.ranges[copy];
            if low > FULL.0 && high < FULL.1 {
                facts.assumes.insert(copy, (low as i64, high as i64));
            }
        }
        facts.find_reservations(f, &stepped);
        facts
    }

    fn step(&mut self, inst: &InstKind, single: &dyn Fn(LocalId) -> bool, copies: &HashMap<usize, (usize, Operand)>) {
        match inst {
            InstKind::Copy { dest, value } if single(*dest) => {
                self.ranges[dest.0] = match copies.get(&dest.0) {
                    Some((count_at, limit)) => {
                        let low = self.ranges[*count_at].0;
                        (low, self.range(limit).1 - 1)
                    }
                    None => self.range(value),
                };
                if let Operand::Local(from) = value {
                    self.length_of[dest.0] = self.length_of[from.0];
                    if self.nonzero.contains(&from.0) {
                        self.nonzero.insert(dest.0);
                    }
                }
            }
            InstKind::Binary { dest, op, lhs, rhs } if single(*dest) => {
                let (a, b) = (self.range(lhs), self.range(rhs));
                let exact = match op {
                    BinOp::Add => Some((a.0 + b.0, a.1 + b.1)),
                    BinOp::Sub => Some((a.0 - b.1, a.1 - b.0)),
                    BinOp::Mul => {
                        let corners = [
                            a.0.saturating_mul(b.0),
                            a.0.saturating_mul(b.1),
                            a.1.saturating_mul(b.0),
                            a.1.saturating_mul(b.1),
                        ];
                        corners.into_iter().min().zip(corners.into_iter().max())
                    }
                    BinOp::FloorDiv => match rhs {
                        Operand::Int(d) if *d > 0 => Some((a.0.div_euclid(i128::from(*d)), a.1.div_euclid(i128::from(*d)))),
                        _ => None,
                    },
                    BinOp::Mod => match rhs {
                        Operand::Int(d) if *d > 0 => Some((0, i128::from(*d) - 1)),
                        _ => None,
                    },
                    _ => None,
                };
                let Some((low, high)) = exact else { return };
                if low >= FULL.0 && high <= FULL.1 && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                    self.safe.insert(dest.0);
                }
                // A check stops anything beyond an int, so the rest of the range still holds.
                self.ranges[dest.0] = (low.max(FULL.0), high.min(FULL.1));
            }
            InstKind::Builtin { dest: Some(dest), name, args, .. } if single(*dest) => match name.as_str() {
                "list.len" => {
                    self.ranges[dest.0] = (0, MOST_ITEMS);
                    if let Some(Operand::Local(list)) = args.first() {
                        if self.fixed_lists.contains(&list.0) {
                            self.length_of[dest.0] = Some(list.0);
                        }
                    }
                }
                "float" => {
                    if let Some(arg) = args.first() {
                        let (low, high) = self.range(arg);
                        if low > 0 || high < 0 {
                            self.nonzero.insert(dest.0);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// `[f(x) for x in range(n)]` becomes an empty list and a loop of appends, so the block
    /// before the loop can make room for all of it at once.
    fn find_reservations(&mut self, f: &Function, stepped: &HashSet<usize>) {
        for (at, block) in f.blocks.iter().enumerate() {
            let Terminator::Jump(BlockId(header)) = block.end else { continue };
            let Some(header) = f.blocks.get(header) else { continue };
            let Terminator::Branch { cond: Operand::Local(cond), then, .. } = &header.end else { continue };
            let test = header.insts.iter().find_map(|inst| match &inst.kind {
                InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(count_at), rhs }
                    if dest == cond && stepped.contains(&count_at.0) =>
                {
                    Some(rhs.clone())
                }
                _ => None,
            });
            let Some(limit) = test else { continue };
            let Some(body) = f.blocks.get(then.0) else { continue };
            let appended = body.insts.iter().find_map(|inst| match &inst.kind {
                InstKind::Builtin { name, args, .. } if name == "list.append" => match args.first() {
                    Some(Operand::Local(list)) => Some(list.0),
                    _ => None,
                },
                _ => None,
            });
            let Some(list) = appended else { continue };
            let made_here = block.insts.iter().any(|inst| {
                matches!(&inst.kind, InstKind::MakeList { dest, items } if dest.0 == list && items.is_empty())
            });
            if made_here {
                self.reserve.insert(at, (list, limit));
            }
        }
    }
}
