//! Rollback in `lock x as ref s:`: a crash inside puts back every value the block changed. The
//! runtime keeps a log for each such lock (`crates/interp/src/embed/guarded/rollback.rs`), and
//! native code tells it what changes: it begins the log when the lock is taken, saves each word
//! before writing it, and keeps the log at a normal end.
//!
//! A `lock ... as ref` is lowered onto a generated `shared Shared__T` with one field, `value`, so
//! a lock to write on a local of such a type is one whose changes are saved. A read lock never
//! saves, and neither does a plain `with` on a `shared` type of the program's own.

use std::collections::HashSet;

use fennec_check::Type;
use fennec_ir::{Function, InstKind, LocalId, Operand};

use crate::emit::Emit;
use crate::kinds::Kind;
use crate::lists::FIELDS_OFFSET;

/// The prefix of the `shared` types `lock` is lowered onto.
const LOCKED: &str = "Shared__";

/// The handles a function locks to write that save their changes: each `lock ... as ref` is
/// lowered onto a handle of its own, so its `unlock` is known by the handle alone.
pub(crate) fn saving_handles(function: &Function) -> HashSet<usize> {
    let locked = |local: &LocalId| {
        matches!(
            function.locals.get(local.0).map(|local| &local.ty),
            Some(Type::Named(name)) if name.starts_with(LOCKED)
        )
    };
    let mut found = HashSet::new();
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        match &inst.kind {
            InstKind::Lock { handle: Operand::Local(local), write: true } if locked(local) => {
                found.insert(local.0);
            }
            InstKind::LockAll { handles } => {
                for (handle, write) in handles {
                    if let Operand::Local(local) = handle
                        && *write
                        && locked(local)
                    {
                        found.insert(local.0);
                    }
                }
            }
            _ => {}
        }
    }
    found
}

impl Emit<'_, '_, '_> {
    /// Whether `handle` is the value of a `lock ... as ref` block, whose changes are saved.
    pub(crate) fn saves_changes(&self, handle: &Operand) -> bool {
        matches!(handle, Operand::Local(local) if self.saving.contains(&local.0))
    }

    /// The lock on `cell` has just been taken: its log begins.
    pub(crate) fn rollback_begin(&mut self, cell: &str) {
        self.declare("declare void @fennec_lock_begin(i64) nounwind");
        let address = self.temp();
        self.line(&format!("{address} = ptrtoint ptr {cell} to i64"));
        self.line(&format!("call void @fennec_lock_begin(i64 {address})"));
    }

    /// The innermost lock that saves its changes ends normally: what it changed is kept.
    pub(crate) fn rollback_kept(&mut self) {
        self.declare("declare void @fennec_lock_kept() nounwind");
        self.line("call void @fennec_lock_kept()");
    }

    /// `block` as an address, and the index of the word at `place` in it, counting its count
    /// word as the first.
    fn word_in(&mut self, block: &str, place: &str) -> (String, String) {
        let (at, from, bytes, word) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{at} = ptrtoint ptr {place} to i64"));
        self.line(&format!("{from} = ptrtoint ptr {block} to i64"));
        self.line(&format!("{bytes} = sub i64 {at}, {from}"));
        self.line(&format!("{word} = lshr i64 {bytes}, 3"));
        (from, word)
    }

    /// Saves the field at `place`, inside `block`, before a store of a `kind` writes it. Gives
    /// back whether the log took over the old value's count, so the store mustn't let go of it.
    pub(crate) fn save_before_store(&mut self, block: &str, place: &str, kind: &Kind) -> bool {
        if matches!(kind, Kind::Str) && self.headers() {
            self.declare("declare void @fennec_lock_save_text(i64, i64, i64, i64, i64) nounwind");
            let (block, word) = self.word_in(block, place);
            let mut words = Vec::with_capacity(3);
            for at in 0..3_u32 {
                let (word_at, old) = (self.offset(place, at * 8), self.temp());
                self.line(&format!("{old} = load i64, ptr {word_at}"));
                words.push(old);
            }
            let [w0, w1, w2] = [0, 1, 2].map(|at| words.get(at).cloned().unwrap_or_default());
            self.line(&format!(
                "call void @fennec_lock_save_text(i64 {block}, i64 {word}, i64 {w0}, i64 {w1}, i64 {w2})"
            ));
            return true;
        }
        let holds = if kind.counted() {
            true
        } else if matches!(kind, Kind::Int | Kind::Float | Kind::Bool) {
            false
        } else {
            // A value of several words, some of which may hold blocks, isn't saved yet.
            return false;
        };
        self.declare("declare void @fennec_lock_save(i64, i64, i64, i64) nounwind");
        let (block, word) = self.word_in(block, place);
        let old = self.temp();
        self.line(&format!("{old} = load i64, ptr {place}"));
        self.line(&format!(
            "call void @fennec_lock_save(i64 {block}, i64 {word}, i64 {old}, i64 {})",
            i64::from(holds)
        ));
        holds
    }

    /// Before a builtin changes in place a value read from a field of a `lock ... as ref` block's
    /// value, like `payer.log.append(x)`, the runtime is told, so the change copies it and a
    /// crash can put the old one back. The value was read as `field = (handle.value).name`.
    pub(crate) fn save_before_change(&mut self, changed: &Operand) -> Result<(), String> {
        let Operand::Local(changed) = changed else { return Ok(()) };
        let Some((handle, path)) = self.read_from_saving(*changed) else { return Ok(()) };
        let (cell, kind) = self.operand(&Operand::Local(handle))?;
        let Kind::Boxed(_) = kind else { return Ok(()) };
        // The cell's fields, then each named step into the value held in place.
        let mut place = self.offset(&cell, FIELDS_OFFSET);
        let mut kind = kind;
        for (step, name) in path.iter().enumerate() {
            let (offset, inner) = self.field(&kind, name)?;
            place = self.offset(&place, offset);
            kind = inner;
            // Only the last step may lead to a block; the steps before stay inside the cell.
            if step + 1 < path.len() && !matches!(kind, Kind::Struct(_)) {
                return Ok(());
            }
        }
        if !kind.counted() {
            return Ok(());
        }
        self.declare("declare void @fennec_lock_save_ref(i64, i64, i64) nounwind");
        let (block, word) = self.word_in(&cell, &place);
        let old = self.temp();
        self.line(&format!("{old} = load i64, ptr {place}"));
        self.line(&format!("call void @fennec_lock_save_ref(i64 {block}, i64 {word}, i64 {old})"));
        Ok(())
    }

    /// Where a local's value was read from in a saving lock's value, in the block being written:
    /// the handle, and the path of fields from it, like `["value", "log"]`.
    fn read_from_saving(&self, local: LocalId) -> Option<(LocalId, Vec<String>)> {
        let (at, index) = self.at;
        let insts = self.function.blocks.get(at)?.insts.get(..index)?;
        let mut path = Vec::new();
        let mut wanted = local;
        for inst in insts.iter().rev() {
            if inst.kind.dest() != Some(wanted) {
                continue;
            }
            let InstKind::GetField { object: Operand::Local(object), field, .. } = &inst.kind else {
                return None;
            };
            path.push(field.clone());
            if self.saving.contains(&object.0) {
                path.reverse();
                return Some((*object, path));
            }
            wanted = *object;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    fn texts(source: &str) -> Result<String, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let (units, _, _) = crate::emit::emit(&fennec_ir::lower(&module, &checked))?;
        Ok(units.into_iter().map(|unit| unit.text).collect())
    }

    /// A lock to write begins a log, saves each field before writing it, saves a list before
    /// appending to it, and keeps the log at its end. A lock to read does none of it.
    #[test]
    fn a_lock_to_write_saves_what_it_changes() -> Result<(), String> {
        let source = "\
struct Account:
    owner: str
    log: list[int]
    balance = 0

def pay(account: shared Account, amount: int):
    lock account as ref payer:
        payer.balance -= amount
        payer.owner = \"bo\"
        payer.log.append(amount)

def main(stdout: Stdout):
    shared ana = Account(owner=\"ana\", log=[], balance=10)
    pay(ana, amount=4)
    lock ana as current:
        stdout.print(current.balance)
";
        let text = texts(source)?;
        let pay = text.split("define").find(|function| function.contains("@\"f_pay\"(")).unwrap_or_default();
        for call in ["begin", "save", "save_text", "save_ref", "kept"] {
            assert!(pay.contains(&format!("call void @fennec_lock_{call}(")), "{call}: {pay}");
        }
        let main =
            text.split("define").find(|function| function.contains("@\"f_main\"(")).unwrap_or_default();
        assert!(!main.contains("@fennec_lock_begin("), "{main}");
        Ok(())
    }
}
