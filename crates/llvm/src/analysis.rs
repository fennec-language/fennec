//! What's known about each function before emitting it: which of its checks can't fail, from
//! `fennec_ir::facts`, and so which functions can fail at all.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_ir::facts::{Facts, Range};
use fennec_ir::{Function, InstKind, LocalId, Operand, Program, Terminator};
use fennec_syntax::{BinOp, UnaryOp};

const FULL: Range = (i64::MIN, i64::MAX);

/// What's known about one function.
pub(crate) struct Analysis {
    /// Locals an add, subtract, multiply or negate makes that can't overflow.
    pub(crate) safe: HashSet<usize>,
    /// Float locals that can't be zero.
    pub(crate) nonzero: HashSet<usize>,
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

fn analyze(function: &Function, changes: &dyn Fn(&str) -> Vec<usize>) -> Analysis {
    let mut facts = Facts::of(function, changes);
    let mut known = Analysis { safe: HashSet::new(), nonzero: HashSet::new() };
    let stepped: HashSet<usize> = function.counts.iter().map(|count| count.count.0).collect();
    for (block, written) in function.blocks.iter().enumerate() {
        for inst in &written.insts {
            match &inst.kind {
                InstKind::Copy { dest, value } => {
                    facts.note_write(*dest, range_of(&facts, value, block));
                    if let Operand::Local(from) = value
                        && known.nonzero.contains(&from.0)
                    {
                        known.nonzero.insert(dest.0);
                    }
                }
                InstKind::Binary { dest, op, lhs, rhs } if function.type_of(lhs) == Some(Type::Int) => {
                    if stepped.contains(&dest.0) && *lhs == Operand::Local(*dest) {
                        known.safe.insert(dest.0);
                        continue;
                    }
                    let (a, b) = (range_of(&facts, lhs, block), range_of(&facts, rhs, block));
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
                    let range = range_of(&facts, value, block);
                    if function.type_of(value) == Some(Type::Int) && range.0 > i64::MIN {
                        known.safe.insert(dest.0);
                        facts.note_write(*dest, (-range.1, -range.0));
                    }
                }
                InstKind::Builtin { dest: Some(dest), name, args, .. } if name == "float" => {
                    let (low, high) = args.first().map_or(FULL, |arg| range_of(&facts, arg, block));
                    if low > 0 || high < 0 {
                        known.nonzero.insert(dest.0);
                    }
                }
                _ => {}
            }
        }
    }
    known
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
        InstKind::Call { .. } => true,
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
