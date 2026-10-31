//! What's known about values while compiling, so native code can leave out checks that can't
//! fail. An SSA value never changes, so a fact about one holds wherever it's used.
//!
//! An int's range comes from constants, lengths, `range` loops and arithmetic on those. A
//! sum whose range fits an int can't overflow, and an index a loop keeps below a list's
//! length can't be out of range.

use std::collections::{HashMap, HashSet};

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::{Block, InstBuilder, InstructionData, MemFlagsData, Opcode, Value, types};
use cranelift_frontend::Variable;
use fennec_ir::facts::{flipped, narrowed_range};
use fennec_syntax::{BinOp, Expr, ExprKind, UnaryOp};

use super::Lowerer;
use crate::types::Ty;

/// The most items anything in memory can have: more than an address space could hold.
pub(super) const MOST_ITEMS: i64 = 1 << 48;

#[derive(Default)]
pub(super) struct Facts {
    /// The lowest and highest an int can be.
    ranges: HashMap<Value, (i64, i64)>,
    /// Floats that can't be zero.
    nonzero: HashSet<Value>,
    /// Pairs where, in the loop bodies being lowered, the first is below the second.
    below: Vec<(Value, Value)>,
    /// What each name that can't change holds, by its variable. Reading the value itself
    /// keeps what's known about it, where reading the variable inside a loop gives a value
    /// the SSA builder hasn't yet worked out is the same.
    fixed: HashMap<usize, Value>,
    /// How long the lists are that names which can't change hold, by their variables.
    lengths: HashMap<usize, Value>,
    /// Ranges narrowed by the branches being lowered, each with the range it replaced.
    narrowed: Vec<(Value, (i64, i64))>,
    /// Strings a `for` loop over text gave: the one shared block for that character.
    pub(super) one_chars: HashSet<Value>,
    /// A block where the stack is known to have room for a call.
    stack_ok: Option<Block>,
}

impl Lowerer<'_, '_> {
    fn resolved(&self, value: Value) -> Value {
        self.b.func.dfg.resolve_aliases(value)
    }

    /// The lowest and highest an int can be.
    pub(super) fn range(&self, value: Value) -> (i64, i64) {
        let value = self.resolved(value);
        if let Some(constant) = self.constant(value) {
            return (constant, constant);
        }
        self.facts.ranges.get(&value).copied().unwrap_or((i64::MIN, i64::MAX))
    }

    pub(super) fn note_range(&mut self, value: Value, range: (i64, i64)) {
        let value = self.resolved(value);
        self.facts.ranges.insert(value, range);
    }

    /// Remembers what a name holds, when it can't change.
    pub(super) fn note_fixed(&mut self, var: Variable, value: Value, ty: Ty) {
        self.facts.fixed.insert(var.index(), value);
        if matches!(ty, Ty::List(_)) {
            let length = self.length_of_list(value);
            self.facts.lengths.insert(var.index(), length);
        }
    }

    /// A name's value, the same one each time when the name can't change.
    pub(super) fn read_var(&mut self, var: Variable) -> Value {
        match self.facts.fixed.get(&var.index()) {
            Some(value) => *value,
            None => self.b.use_var(var),
        }
    }

    /// The length of the list a name that can't change holds.
    pub(super) fn fixed_length(&self, var: Variable) -> Option<Value> {
        self.facts.lengths.get(&var.index()).copied()
    }

    /// Forgets what's known about a name given a new value.
    pub(super) fn forget_var(&mut self, var: Variable) {
        self.facts.fixed.remove(&var.index());
        self.facts.lengths.remove(&var.index());
    }

    /// In the loop body about to be lowered, `at` is below `end`.
    pub(super) fn push_below(&mut self, at: Value, end: Value) {
        let pair = (self.resolved(at), self.resolved(end));
        self.facts.below.push(pair);
    }

    pub(super) fn pop_below(&mut self) {
        self.facts.below.pop();
    }

    /// Whether `at` is known to be a position from the start of something `length` long.
    pub(super) fn known_inside(&self, at: Value, length: Value) -> bool {
        let pair = (self.resolved(at), self.resolved(length));
        self.range(at).0 >= 0 && self.facts.below.contains(&pair)
    }

    /// Narrows what's known from a condition being true or false, until `widen`.
    pub(super) fn narrow(&mut self, cond: &Expr, holds: bool) {
        match &cond.kind {
            ExprKind::Unary { op: UnaryOp::Not, expr } => self.narrow(expr, !holds),
            ExprKind::Binary { op: BinOp::And, lhs, rhs } if holds => {
                self.narrow(lhs, true);
                self.narrow(rhs, true);
            }
            ExprKind::Binary { op: BinOp::Or, lhs, rhs } if !holds => {
                self.narrow(lhs, false);
                self.narrow(rhs, false);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                if let (Some(name), Some(limit)) = (self.fixed_int(lhs), literal_int(rhs)) {
                    self.narrow_to(name, *op, limit, holds);
                } else if let (Some(limit), Some(name), Some(flipped)) =
                    (literal_int(lhs), self.fixed_int(rhs), flipped(*op))
                {
                    self.narrow_to(name, flipped, limit, holds);
                }
            }
            _ => {}
        }
    }

    /// The value of an int name that can't change.
    fn fixed_int(&self, expr: &Expr) -> Option<Value> {
        let ExprKind::Name(name) = &expr.kind else { return None };
        let local = self.local(name).filter(|local| local.ty == Ty::Int && !local.is_mut)?;
        self.facts.fixed.get(&local.var?.index()).map(|value| self.resolved(*value))
    }

    fn narrow_to(&mut self, value: Value, op: BinOp, limit: i64, holds: bool) {
        let old = self.range(value);
        let Some(new) = narrowed_range(old, op, limit, holds) else { return };
        self.facts.narrowed.push((value, old));
        self.facts.ranges.insert(value, new);
    }

    /// A mark to `widen` back to.
    pub(super) fn narrow_mark(&self) -> usize {
        self.facts.narrowed.len()
    }

    /// Undoes what's been narrowed since `mark`, leaving where it was true.
    pub(super) fn widen(&mut self, mark: usize) {
        while self.facts.narrowed.len() > mark {
            if let Some((value, old)) = self.facts.narrowed.pop() {
                self.facts.ranges.insert(value, old);
            }
        }
    }

    /// Whether a call here needs its stack check: the stack pointer doesn't move within a
    /// function, so a path that has checked once needn't again.
    pub(super) fn stack_unchecked(&self) -> bool {
        self.facts.stack_ok.is_none() || self.facts.stack_ok != self.b.current_block()
    }

    pub(super) fn note_stack_checked(&mut self) {
        self.facts.stack_ok = self.b.current_block();
    }

    /// Going on in `next`, a block only `from` leads to, the stack has room if it had in `from`.
    pub(super) fn carry_stack(&mut self, from: Option<Block>, next: Block) {
        if self.facts.stack_ok.is_some() && self.facts.stack_ok == from {
            self.facts.stack_ok = Some(next);
        }
    }

    /// An int as a float, remembering when it can't be zero.
    pub(super) fn int_to_float(&mut self, x: Value) -> Value {
        let (low, high) = self.range(x);
        let float = self.b.ins().fcvt_from_sint(types::F64, x);
        if low > 0 || high < 0 {
            let float = self.resolved(float);
            self.facts.nonzero.insert(float);
        }
        float
    }

    /// Whether a float is known not to be zero.
    pub(super) fn nonzero_float(&self, value: Value) -> bool {
        let value = self.resolved(value);
        if self.facts.nonzero.contains(&value) {
            return true;
        }
        let Some(inst) = self.b.func.dfg.value_def(value).inst() else { return false };
        match self.b.func.dfg.insts[inst] {
            InstructionData::UnaryIeee64 { opcode: Opcode::F64const, imm } => imm.bits() << 1 != 0,
            _ => false,
        }
    }

    /// The range of `a op b` if nothing stopped it overflowing.
    pub(super) fn exact_range(&self, op: BinOp, a: Value, b: Value) -> Option<(i128, i128)> {
        let (a_low, a_high) = self.range(a);
        let (b_low, b_high) = self.range(b);
        let (a_low, a_high, b_low, b_high) =
            (i128::from(a_low), i128::from(a_high), i128::from(b_low), i128::from(b_high));
        match op {
            BinOp::Add => Some((a_low + b_low, a_high + b_high)),
            BinOp::Sub => Some((a_low - b_high, a_high - b_low)),
            BinOp::Mul => {
                let corners = [a_low * b_low, a_low * b_high, a_high * b_low, a_high * b_high];
                Some((corners.into_iter().min()?, corners.into_iter().max()?))
            }
            _ => None,
        }
    }

    /// The range of `a // b` or `a % b` when `b` is a positive constant.
    pub(super) fn division_range(&self, op: BinOp, a: Value, b: Value) -> Option<(i64, i64)> {
        let divisor = self.constant(b).filter(|divisor| *divisor > 0)?;
        let (low, high) = self.range(a);
        match op {
            BinOp::FloorDiv => Some((low.div_euclid(divisor), high.div_euclid(divisor))),
            BinOp::Mod => Some((0, divisor - 1)),
            _ => None,
        }
    }

    /// How long a list is, from its count of items.
    pub(super) fn load_length(&mut self, block: Value, offset: i32) -> Value {
        let length = self.b.ins().load(types::I64, MemFlagsData::trusted(), block, offset);
        self.note_range(length, (0, MOST_ITEMS));
        length
    }
}

/// An int written in the program.
fn literal_int(expr: &Expr) -> Option<i64> {
    match &expr.kind {
        ExprKind::Int(n) => Some(*n),
        ExprKind::Unary { op: UnaryOp::Neg, expr } => literal_int(expr)?.checked_neg(),
        _ => None,
    }
}

/// A range that can't be wider than an int's, once a check has stopped an overflow.
pub(super) fn clamped((low, high): (i128, i128)) -> (i64, i64) {
    let clamp = |n: i128| i64::try_from(n.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or(0);
    (clamp(low), clamp(high))
}

/// Whether a range fits an int, so what makes it can't overflow.
pub(super) fn fits((low, high): (i128, i128)) -> bool {
    low >= i128::from(i64::MIN) && high <= i128::from(i64::MAX)
}
