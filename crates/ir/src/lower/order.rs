//! Evaluation runs left to right, so an operand keeps the value it had when it was read, even if
//! one worked out after it changes the local it names. Most never do, and copying each just in
//! case would cost every call, so what's read is noted, and only a local that really did change
//! is copied, back where it was read.

use fennec_syntax::Span;

use super::Lowering;
use crate::{BlockId, Inst, InstKind, LocalId, Operand};

/// One operand, as it was read.
struct Read {
    value: Operand,
    block: BlockId,
    at: usize,
    /// How many changes had been noted by then.
    changes: usize,
}

/// The operands of one thing being worked out, in the order they're written.
#[derive(Default)]
pub(super) struct Reads(Vec<Read>);

impl Reads {
    /// Adds an operand just worked out.
    pub(super) fn add(&mut self, lowering: &Lowering<'_>, value: Operand) {
        let at = lowering.blocks.get(lowering.current.0).map_or(0, |block| block.insts.len());
        self.0.push(Read { value, block: lowering.current, at, changes: lowering.changed.len() });
    }

    /// Adds an operand that's the place itself, which a change is meant to reach.
    pub(super) fn add_place(&mut self, value: Operand) {
        self.0.push(Read { value, block: BlockId(0), at: 0, changes: usize::MAX });
    }

    /// The operands in order, each as it was when it was read.
    pub(super) fn finish(self, lowering: &mut Lowering<'_>, span: Span) -> Vec<Operand> {
        let mut values = Vec::with_capacity(self.0.len());
        // Last first, so a copy put in leaves where the earlier ones were read as it was.
        for read in self.0.into_iter().rev() {
            values.push(lowering.as_read(read, span));
        }
        values.reverse();
        values
    }
}

impl Lowering<'_> {
    /// Notes what an instruction changes.
    pub(super) fn note_change(&mut self, kind: &InstKind) {
        let changed = match kind {
            InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } => Some(*object),
            InstKind::Builtin { args, changes: true, .. } => match args.first() {
                Some(Operand::Local(local)) => Some(*local),
                _ => None,
            },
            _ => kind.dest().filter(|dest| self.locals.get(dest.0).is_some_and(|local| local.name.is_some())),
        };
        self.changed.extend(changed);
    }

    /// Notes that a call is given a local to change.
    pub(super) fn note_changed_place(&mut self, local: LocalId) {
        self.changed.push(local);
    }

    /// The value of two operands, the first kept as it was if working out the second changes it.
    pub(super) fn both(
        &mut self,
        lhs: Operand,
        rhs: impl FnOnce(&mut Self) -> super::Lowered<Operand>,
        span: Span,
    ) -> super::Lowered<(Operand, Operand)> {
        let mut reads = Reads::default();
        reads.add(self, lhs);
        let rhs = rhs(self)?;
        reads.add(self, rhs);
        let mut values = reads.finish(self, span).into_iter();
        match (values.next(), values.next()) {
            (Some(lhs), Some(rhs)) => Ok((lhs, rhs)),
            _ => Err("an operand went missing".to_owned()),
        }
    }

    fn as_read(&mut self, read: Read, span: Span) -> Operand {
        let Operand::Local(local) = read.value else { return read.value };
        if !self.changed.get(read.changes..).is_some_and(|since| since.contains(&local)) {
            return read.value;
        }
        let ty = self.locals.get(local.0).map(|held| held.ty.clone());
        let Some(ty) = ty else { return read.value };
        let dest = self.temporary(ty);
        let copy = InstKind::Copy { dest, value: read.value };
        if let Some(block) = self.blocks.get_mut(read.block.0) {
            let at = read.at.min(block.insts.len());
            block.insts.insert(at, Inst { kind: copy, span });
        }
        Operand::Local(dest)
    }
}
