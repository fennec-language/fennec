//! Rollback in `lock x as ref s:`: a crash inside puts back every value the block changed. The
//! runtime keeps a log for each such lock (`crates/rt/src/embed/guarded/rollback.rs`), and
//! native code tells it what changes: it begins the log when the lock is taken, saves each word
//! before writing it, and keeps the log at a normal end.
//!
//! A `lock ... as ref` is lowered onto a generated `shared Shared__T` with one field, `value`, so
//! a lock to write on a local of such a type is one whose changes are saved. A read lock never
//! saves, and neither does a plain `with` on a `shared` type of the program's own.

use std::fmt::Write as _;

use fennec_check::Type;
use fennec_ir::{Function, InstKind, LocalId, Operand};

use crate::emit::Emit;
use crate::kinds::Kind;
use crate::lists::FIELDS_OFFSET;

/// The prefix of the `shared` types `lock` is lowered onto.
const LOCKED: &str = "Shared__";

/// What a word of a lock's own cell holds, for the runtime to know what it's putting back.
#[derive(Clone, Copy)]
enum Word {
    /// A number, or a bool.
    Plain,
    /// A block's address.
    Block,
    /// The first of a string header's three words.
    Header,
}

/// The handles a function locks to write that save their changes, in the order they're first
/// locked: each `lock ... as ref` is lowered onto a handle of its own, so its `unlock` is known by
/// the handle alone.
pub(crate) fn saving_handles(function: &Function) -> Vec<usize> {
    let locked = |local: &LocalId| {
        matches!(
            function.locals.get(local.0).map(|local| &local.ty),
            Some(Type::Named(name)) if name.starts_with(LOCKED)
        )
    };
    let mut found = Vec::new();
    let mut add = |local: usize| {
        if !found.contains(&local) {
            found.push(local);
        }
    };
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        match &inst.kind {
            InstKind::Lock { handle: Operand::Local(local), write: true } if locked(local) => add(local.0),
            InstKind::LockAll { handles } => {
                for (handle, write) in handles {
                    if let Operand::Local(local) = handle
                        && *write
                        && locked(local)
                    {
                        add(local.0);
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// The handles a function locks to read, which a crash gives back, in the order they're first
/// locked, beside the handles that save their changes, and the cleanups a crash runs, each a
/// closure from a `lend x else:`, in the order they're first set: a crash goes through them all
/// innermost first.
pub(crate) fn given_back_on_crash(function: &Function) -> Vec<usize> {
    let saving = saving_handles(function);
    let mut found = Vec::new();
    let mut add = |handle: &Operand, write: bool| {
        if let Operand::Local(local) = handle
            && (!write || saving.contains(&local.0))
            && !found.contains(&local.0)
        {
            found.push(local.0);
        }
    };
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        match &inst.kind {
            InstKind::Lock { handle, write } => add(handle, *write),
            InstKind::LockAll { handles } => handles.iter().for_each(|(handle, write)| add(handle, *write)),
            InstKind::CrashCleanup { cleanup } => add(cleanup, false),
            _ => {}
        }
    }
    found
}

/// A plain save, written into this thread's buffer as the runtime would (`rollback::Buffer` in
/// `crates/rt`), inline. Its record is four words: how many saves the buffer holds, how many it
/// has room for, where they are, and where the innermost lock's start. A save of the same word as
/// the last of this lock's is the same place written again, which needs nothing. With no room, the
/// runtime saves it, making room.
const SAVE_PLAIN: &str =
    "define internal void @fen_lock_save(i64 %record, i64 %block, i64 %word, i64 %old) #2 {
entry:
  %rec = inttoptr i64 %record to ptr
  %len = load i64, ptr %rec
  %room.at = getelementptr i64, ptr %rec, i64 1
  %room = load i64, ptr %room.at
  %saves.at = getelementptr i64, ptr %rec, i64 2
  %saves.address = load i64, ptr %saves.at
  %saves = inttoptr i64 %saves.address to ptr
  %start.at = getelementptr i64, ptr %rec, i64 3
  %start = load i64, ptr %start.at
  %any = icmp ugt i64 %len, %start
  br i1 %any, label %last, label %check
last:
  %last.index = sub i64 %len, 1
  %last.first = mul i64 %last.index, 3
  %last.at = getelementptr i64, ptr %saves, i64 %last.first
  %last.block = load i64, ptr %last.at
  %last.word.at = getelementptr i64, ptr %last.at, i64 1
  %last.word = load i64, ptr %last.word.at
  %same.block = icmp eq i64 %last.block, %block
  %same.word = icmp eq i64 %last.word, %word
  %same = and i1 %same.block, %same.word
  br i1 %same, label %done, label %check
check:
  %has.room = icmp ult i64 %len, %room
  br i1 %has.room, label %write, label %call, !prof !8
write:
  %first = mul i64 %len, 3
  %at = getelementptr i64, ptr %saves, i64 %first
  store i64 %block, ptr %at
  %word.at = getelementptr i64, ptr %at, i64 1
  store i64 %word, ptr %word.at
  %old.at = getelementptr i64, ptr %at, i64 2
  store i64 %old, ptr %old.at
  %next = add i64 %len, 1
  store i64 %next, ptr %rec
  br label %done
call:
  call void @fennec_lock_save(i64 %block, i64 %word, i64 %old, i64 0)
  br label %done
done:
  ret void
}

";

impl Emit<'_, '_, '_> {
    /// Whether a crash gives back `handle`'s lock with nothing to put back: it's a read lock.
    pub(crate) fn read_lock_given_back(&self, handle: &Operand) -> bool {
        !self.saves_changes(handle)
            && matches!(handle, Operand::Local(local) if self.on_crash.contains(&local.0))
    }

    /// Notes whether `handle`'s read lock is held, for a crash to give it back.
    pub(crate) fn note_read_lock(&mut self, handle: &Operand, held: bool) {
        if let Some(flag) = self.held_flag(handle) {
            self.line(&format!("store i1 {held}, ptr {flag}"));
        }
    }

    /// Whether `handle` is the value of a `lock ... as ref` block, whose changes are saved.
    pub(crate) fn saves_changes(&self, handle: &Operand) -> bool {
        matches!(handle, Operand::Local(local) if self.saving.contains(&local.0))
    }

    /// The flag saying `handle`'s lock is held, for a crash to know to put back what it changed.
    fn held_flag(&mut self, handle: &Operand) -> Option<String> {
        let Operand::Local(local) = handle else { return None };
        if let Some(slot) = self.saving_slots.get(&local.0) {
            return Some(slot.clone());
        }
        let slot = self.flag_slot();
        self.saving_slots.insert(local.0, slot.clone());
        Some(slot)
    }

    /// The lock on `handle`'s cell has just been taken: its log begins.
    pub(crate) fn rollback_begin(&mut self, handle: &Operand, cell: &str) {
        self.declare("declare i64 @fennec_lock_begin(i64) nounwind");
        let (address, record) = (self.temp(), self.temp());
        self.line(&format!("{address} = ptrtoint ptr {cell} to i64"));
        self.line(&format!("{record} = call i64 @fennec_lock_begin(i64 {address})"));
        let slot = self.record_slot();
        self.line(&format!("store i64 {record}, ptr {slot}"));
        if let Some(flag) = self.held_flag(handle) {
            self.line(&format!("store i1 true, ptr {flag}"));
        }
    }

    /// The innermost lock that saves its changes ends normally: what it changed is kept.
    pub(crate) fn rollback_kept(&mut self, handle: &Operand) {
        if let Some(flag) = self.held_flag(handle) {
            self.line(&format!("store i1 false, ptr {flag}"));
        }
        self.declare("declare void @fennec_lock_kept() nounwind");
        self.line("call void @fennec_lock_kept()");
    }

    /// `lock.roll_back(handle)`: a failure is leaving this lock's block, so what the block changed
    /// is put back, as for a crash, and the unlock just after it keeps nothing. Any other lock has
    /// saved nothing to put back.
    pub(crate) fn roll_back_on_failure(&mut self, handle: &Operand) -> Result<(), String> {
        if !self.saves_changes(handle) {
            return Ok(());
        }
        let Operand::Local(local) = handle else { return Ok(()) };
        self.undo_changes(handle)?;
        self.rolled_back.insert(local.0);
        Ok(())
    }

    /// Whether `handle`'s block has just had its changes put back, so there's nothing to keep.
    pub(crate) fn take_rolled_back(&mut self, handle: &Operand) -> bool {
        matches!(handle, Operand::Local(local) if self.rolled_back.remove(&local.0))
    }

    /// Puts back what `handle`'s block changed: in the runtime's tables, then its own cell.
    fn undo_changes(&mut self, handle: &Operand) -> Result<(), String> {
        self.declare("declare void @fennec_lock_undo() nounwind");
        self.declare("declare i64 @fennec_lock_undo_word(i64, i64, i64) nounwind");
        if let Some(flag) = self.held_flag(handle) {
            self.line(&format!("store i1 false, ptr {flag}"));
        }
        self.line("call void @fennec_lock_undo()");
        let (cell, kind) = self.operand(handle)?;
        let mut words = Vec::new();
        self.cell_words(&kind, &mut words)?;
        for (offset, word) in words {
            self.undo_word(&cell, offset, word);
        }
        Ok(())
    }

    /// Where a crash leaves the function: each `lock ... as ref` block still held, the last taken
    /// first, has what it changed put back, then its lock given back, before the failure goes on.
    /// A read lock still held is only given back.
    /// The runtime puts back the words of blocks in its tables, and native code the words of the
    /// lock's own cell, which it's told one at a time.
    pub(crate) fn roll_back_on_crash(&mut self) -> Result<(), String> {
        for local in self.on_crash.clone().into_iter().rev() {
            let handle = Operand::Local(LocalId(local));
            if self.is_crash_cleanup(local) {
                self.run_crash_cleanup(&handle)?;
                continue;
            }
            let Some(flag) = self.held_flag(&handle) else { continue };
            let (held, undo, next) = (self.temp(), self.label("crash_undo"), self.label("crash_next"));
            self.line(&format!("{held} = load i1, ptr {flag}"));
            self.line(&format!("br i1 {held}, label %{undo}, label %{next}"));
            self.start_block(&undo);
            if self.saves_changes(&handle) {
                self.undo_changes(&handle)?;
            } else {
                self.line(&format!("store i1 false, ptr {flag}"));
            }
            self.give_back_on_crash(&handle)?;
            self.line(&format!("br label %{next}"));
            self.start_block(&next);
        }
        Ok(())
    }

    /// Whether `local` is a cleanup a crash runs, a closure, where the rest a crash goes through
    /// are lock handles.
    fn is_crash_cleanup(&self, local: usize) -> bool {
        matches!(self.kind_of(LocalId(local)), Ok(Kind::Closure(..)))
    }

    /// `CrashCleanup` and `CrashCleanupDone`: a crash from here on runs `cleanup`, or no longer
    /// does, as the block it's for ends or its failure runs the cleanup itself.
    pub(crate) fn crash_cleanup(&mut self, cleanup: &Operand, set: bool) {
        if let Some(flag) = self.held_flag(cleanup) {
            self.line(&format!("store i1 {set}, ptr {flag}"));
        }
    }

    /// Runs `cleanup`, a closure that takes nothing, on the way out after a crash, when the block
    /// it's for hasn't ended. The crash is held aside meanwhile, so the cleanup's own calls run as
    /// normal, and goes on after; a failure or crash of the cleanup's own doesn't replace it. The
    /// cleanup gives back how it failed, as text, or nothing, which the runtime notes on the crash.
    fn run_crash_cleanup(&mut self, cleanup: &Operand) -> Result<(), String> {
        let Some(flag) = self.held_flag(cleanup) else { return Ok(()) };
        let (closure, kind) = self.operand(cleanup)?;
        let Kind::Closure(params, returns) = kind else {
            return Err("a crash's cleanup that isn't a closure".to_owned());
        };
        if !params.is_empty() || !matches!(*returns, Kind::Str) {
            return Err("a crash's cleanup that takes something, or gives back other than text".to_owned());
        }
        self.declare("declare i64 @fennec_crash_hold() nounwind");
        self.declare("declare void @fennec_crash_resume(i64) nounwind");
        self.declare("declare void @fennec_crash_note(i64, i64) nounwind");
        let (held, run, next) = (self.temp(), self.label("crash_cleanup"), self.label("crash_cleaned"));
        self.line(&format!("{held} = load i1, ptr {flag}"));
        self.line(&format!("br i1 {held}, label %{run}, label %{next}"));
        self.start_block(&run);
        self.line(&format!("store i1 false, ptr {flag}"));
        let number = self.temp();
        self.line(&format!("{number} = call i64 @fennec_crash_hold()"));
        self.line("store ptr null, ptr %err");
        let (said, failed) = self.call_closure_unchecked(&closure, &returns, &[]);
        // A cleanup that crashed too, or failed some way it can't, gave back nothing to read: the
        // runtime says its crash on resuming.
        let flag_at = self.failure_flag();
        let (flag, crashed, broke) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{flag} = load i8, ptr {flag_at}"));
        self.line(&format!("{crashed} = icmp ne i8 {flag}, 0"));
        self.line(&format!("{broke} = or i1 {failed}, {crashed}"));
        let (noted, resume) = (self.label("crash_noted"), self.label("crash_resume"));
        self.line(&format!("br i1 {broke}, label %{resume}, label %{noted}"));
        self.start_block(&noted);
        let said = if returns.in_memory() {
            let header = self.temp();
            self.line(&format!("{header} = load {}, ptr {said}", self.text_of(&returns)));
            header
        } else {
            said
        };
        let block = if self.headers() {
            let block = self.owned_text_block(&said);
            self.release_kind(&said, &returns);
            block
        } else {
            said
        };
        let address = self.temp();
        self.line(&format!("{address} = ptrtoint ptr {block} to i64"));
        self.line(&format!("call void @fennec_crash_note(i64 {number}, i64 {address})"));
        self.line(&format!("br label %{resume}"));
        self.start_block(&resume);
        self.line(&format!("call void @fennec_crash_resume(i64 {number})"));
        self.line(&format!("br label %{next}"));
        self.start_block(&next);
        Ok(())
    }

    /// Puts back the word at `offset` in the lock's own cell, as the runtime says it was.
    fn undo_word(&mut self, cell: &str, offset: u32, word: Word) {
        let at = |emit: &mut Self, extra: u32| {
            let place = emit.offset(cell, offset + extra * 8);
            let index = i64::from((offset + extra * 8) / 8);
            (place, index)
        };
        let one = |emit: &mut Self, extra: u32, holds: &str| {
            let (place, index) = at(emit, extra);
            let (now, back) = (emit.temp(), emit.temp());
            emit.line(&format!("{now} = load i64, ptr {place}"));
            emit.line(&format!(
                "{back} = call i64 @fennec_lock_undo_word(i64 {index}, i64 {now}, i64 {holds})"
            ));
            emit.line(&format!("store i64 {back}, ptr {place}"));
        };
        match word {
            Word::Plain => one(self, 0, "0"),
            Word::Block => one(self, 0, "1"),
            Word::Header => {
                // The first word holds a block when the last's top bit is clear.
                let (last_at, _) = at(self, 2);
                let (last, in_block, holds) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{last} = load i64, ptr {last_at}"));
                self.line(&format!("{in_block} = icmp sge i64 {last}, 0"));
                self.line(&format!("{holds} = zext i1 {in_block} to i64"));
                one(self, 0, &holds);
                one(self, 1, "0");
                one(self, 2, "0");
            }
        }
    }

    /// The words of a `lock ... as ref` block's cell that its saves can name, by their place from
    /// the block's start: the cell's fields, then the fields of a struct held in place. A word of a
    /// value of a kind not saved isn't named, and stays as it is.
    fn cell_words(&self, kind: &Kind, words: &mut Vec<(u32, Word)>) -> Result<(), String> {
        let (Kind::Boxed(id) | Kind::Struct(id)) = kind else { return Ok(()) };
        let names: Vec<String> = self.layout_of(*id)?.fields.iter().map(|field| field.name.clone()).collect();
        let base = if matches!(kind, Kind::Boxed(_)) { FIELDS_OFFSET } else { 0 };
        for name in names {
            let (offset, inner) = self.field(kind, &name)?;
            self.words_of(&inner, base + offset, words)?;
        }
        Ok(())
    }

    fn words_of(&self, kind: &Kind, at: u32, words: &mut Vec<(u32, Word)>) -> Result<(), String> {
        match kind {
            Kind::Int | Kind::Float | Kind::Bool | Kind::U64 | Kind::F32 => words.push((at, Word::Plain)),
            Kind::Str if self.headers() => words.push((at, Word::Header)),
            // An enum holding nothing counted is only words.
            Kind::Enum(_) if !self.holds_inline(kind) => {
                let size = kind.size(self.module.layouts);
                words.extend((0..size / 8).map(|word| (at + word * 8, Word::Plain)));
            }
            Kind::Struct(id) => {
                let names: Vec<String> =
                    self.layout_of(*id)?.fields.iter().map(|field| field.name.clone()).collect();
                for name in names {
                    let (offset, inner) = self.field(kind, &name)?;
                    self.words_of(&inner, at + offset, words)?;
                }
            }
            _ if kind.counted() => words.push((at, Word::Block)),
            _ => {}
        }
        Ok(())
    }

    /// The slot holding the address of this thread's record of plain saves, which each lock that
    /// saves its changes stores when it's taken. It starts at 0, so a save no lock's begin came
    /// before would fault at once rather than read whatever the slot held. Every save comes after
    /// one today, which LLVM sees, so it drops the store.
    fn record_slot(&mut self) -> String {
        if let Some(slot) = &self.lock_record {
            return slot.clone();
        }
        let at = self.label("%place");
        let _ = writeln!(self.allocas, "  {at} = alloca i64");
        let _ = writeln!(self.allocas, "  store i64 0, ptr {at}");
        self.lock_record = Some(at.clone());
        at
    }

    /// Saves `old`, the word at `word` in `block`, which holds no block, before it changes: in this
    /// thread's buffer of saves, without a call while it has room (see `fen_lock_save`).
    pub(crate) fn save_plain(&mut self, block: &str, word: &str, old: &str) {
        self.declare("declare void @fennec_lock_save(i64, i64, i64, i64) nounwind");
        self.module.add_helper("fen_lock_save", SAVE_PLAIN.to_owned());
        let (slot, record) = (self.record_slot(), self.temp());
        self.line(&format!("{record} = load i64, ptr {slot}"));
        self.line(&format!("call void @fen_lock_save(i64 {record}, i64 {block}, i64 {word}, i64 {old})"));
    }

    /// `block` as an address, and the index of the word at `place` in it, counting its count
    /// word as the first.
    pub(crate) fn word_in(&mut self, block: &str, place: &str) -> (String, String) {
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
        } else if matches!(kind, Kind::Int | Kind::Float | Kind::Bool | Kind::U64 | Kind::F32) {
            false
        } else {
            // A value of several words is saved a word at a time. The store then lets go of what
            // it holds as usual, and the log has counts of its own.
            let _ = self.save_words(block, place, kind);
            return false;
        };
        let (block, word) = self.word_in(block, place);
        let old = self.temp();
        self.line(&format!("{old} = load i64, ptr {place}"));
        if holds {
            self.declare("declare void @fennec_lock_save(i64, i64, i64, i64) nounwind");
            self.line(&format!("call void @fennec_lock_save(i64 {block}, i64 {word}, i64 {old}, i64 1)"));
        } else {
            self.save_plain(&block, &word, &old);
        }
        holds
    }

    /// Saves each word of the value at `place`, inside `block`, keeping a count of each block in it
    /// for the log. An enum holding blocks is saved by its case, read where it's written: which of
    /// its words hold blocks depends on it.
    fn save_words(&mut self, block: &str, place: &str, kind: &Kind) -> Result<(), String> {
        match kind {
            Kind::Struct(id) => {
                let fields: Vec<(u32, Option<Kind>)> = self
                    .layout_of(*id)?
                    .fields
                    .iter()
                    .map(|field| (field.offset, Kind::from_ty(self.module.layouts, field.ty)))
                    .collect();
                for (offset, field) in fields {
                    let field = field.ok_or("a field this backend doesn't take yet")?;
                    let at = self.offset(place, offset);
                    self.save_words(block, &at, &field)?;
                }
                Ok(())
            }
            Kind::Enum(id) if self.holds_inline(kind) => self.save_case_words(block, place, *id),
            _ => {
                let mut words = Vec::new();
                self.words_of(kind, 0, &mut words)?;
                for (offset, word) in words {
                    let at = self.offset(place, offset);
                    self.save_word(block, &at, word);
                }
                Ok(())
            }
        }
    }

    /// Saves an enum held in place that holds blocks: its tag, then the words of the case it's in.
    fn save_case_words(&mut self, block: &str, place: &str, id: u32) -> Result<(), String> {
        let cases: Vec<Vec<(u32, Option<Kind>)>> = self
            .layout_of(id)?
            .cases
            .as_ref()
            .ok_or("an enum with no cases")?
            .iter()
            .map(|case| {
                case.fields
                    .iter()
                    .map(|field| (field.offset, Kind::from_ty(self.module.layouts, field.ty)))
                    .collect()
            })
            .collect();
        self.save_word(block, place, Word::Plain);
        let tag = self.temp();
        self.line(&format!("{tag} = load i64, ptr {place}"));
        let done = self.label("saved_case");
        let labels: Vec<String> = cases.iter().map(|_| self.label("save_case")).collect();
        let arms: Vec<String> = labels
            .iter()
            .enumerate()
            .map(|(number, label)| format!("i64 {number}, label %{label}"))
            .collect();
        self.line(&format!("switch i64 {tag}, label %{done} [ {} ]", arms.join(" ")));
        for (fields, label) in cases.iter().zip(&labels) {
            self.start_block(label);
            for (offset, field) in fields {
                let field = field.clone().ok_or("a field this backend doesn't take yet")?;
                let at = self.offset(place, *offset);
                self.save_words(block, &at, &field)?;
            }
            self.line(&format!("br label %{done}"));
        }
        self.start_block(&done);
        Ok(())
    }

    /// Saves one word of what a lock holds, at `at` inside `block`, as `word` says it's held.
    fn save_word(&mut self, block: &str, at: &str, word: Word) {
        let (from, index) = self.word_in(block, at);
        match word {
            Word::Plain => {
                let old = self.temp();
                self.line(&format!("{old} = load i64, ptr {at}"));
                self.save_plain(&from, &index, &old);
            }
            Word::Block => {
                self.declare("declare void @fennec_lock_save_ref(i64, i64, i64) nounwind");
                let old = self.temp();
                self.line(&format!("{old} = load i64, ptr {at}"));
                self.line(&format!("call void @fennec_lock_save_ref(i64 {from}, i64 {index}, i64 {old})"));
            }
            Word::Header => {
                self.declare("declare void @fennec_lock_save_text(i64, i64, i64, i64, i64) nounwind");
                let header = self.temp();
                self.line(&format!("{header} = load {}, ptr {at}", crate::headers::HEADER));
                // The log takes over a count of its block, so it has one of its own.
                self.retain_value(&header, &Kind::Str);
                let mut held = Vec::with_capacity(3);
                for part in 0..3_u32 {
                    let (part_at, old) = (self.offset(at, part * 8), self.temp());
                    self.line(&format!("{old} = load i64, ptr {part_at}"));
                    held.push(old);
                }
                let [w0, w1, w2] = [0, 1, 2].map(|part| held.get(part).cloned().unwrap_or_default());
                self.line(&format!(
                    "call void @fennec_lock_save_text(i64 {from}, i64 {index}, i64 {w0}, i64 {w1}, i64 {w2})"
                ));
            }
        }
    }

    /// Before a write reaches along `path` from a local into what a `lock ... as ref` block's value
    /// holds: the first block on the way is saved, so the write copies it and a crash or failure
    /// can put the old one back, and if there's none, the words the path comes to.
    pub(crate) fn save_along(&mut self, object: LocalId, path: &[String]) -> Result<(), String> {
        let (handle, whole) = if self.saving.contains(&object.0) {
            (object, path.to_vec())
        } else if let Some((handle, mut along)) = self.read_from_saving(object) {
            along.extend_from_slice(path);
            (handle, along)
        } else {
            return Ok(());
        };
        let (cell, kind) = self.operand(&Operand::Local(handle))?;
        if !matches!(kind, Kind::Boxed(_)) {
            return Ok(());
        }
        let (mut place, mut kind) = (self.offset(&cell, FIELDS_OFFSET), kind);
        for name in &whole {
            let (offset, inner) = self.field(&kind, name)?;
            place = self.offset(&place, offset);
            kind = inner;
            if kind.counted() {
                self.declare("declare void @fennec_lock_save_ref(i64, i64, i64) nounwind");
                let (from, index) = self.word_in(&cell, &place);
                let old = self.temp();
                self.line(&format!("{old} = load i64, ptr {place}"));
                self.line(&format!("call void @fennec_lock_save_ref(i64 {from}, i64 {index}, i64 {old})"));
                return Ok(());
            }
            if !matches!(kind, Kind::Struct(_)) {
                break;
            }
        }
        self.save_words(&cell, &place, &kind)
    }

    /// For a put into the dict at `object.path` under a lock that saves what it changes, written in
    /// place: where the dict's address is, and an `i1` that holds when every block on the way to it
    /// from the lock's cell has one owner, the dict's own included. `None`, writing nothing, when
    /// the way there isn't only fields of blocks, and of structs held in place in them.
    pub(crate) fn dict_alone_along(
        &mut self,
        object: LocalId,
        path: &[String],
    ) -> Result<Option<(String, String)>, String> {
        let (handle, whole) = if self.saving.contains(&object.0) {
            (object, path.to_vec())
        } else if let Some((handle, mut along)) = self.read_from_saving(object) {
            along.extend_from_slice(path);
            (handle, along)
        } else {
            return Ok(None);
        };
        let cell_kind = self.kind_of(handle)?.clone();
        if !matches!(cell_kind, Kind::Boxed(_)) {
            return Ok(None);
        }
        // What each step reaches, worked out before anything's written.
        let mut steps = Vec::with_capacity(whole.len());
        let mut kind = cell_kind;
        for (step, name) in whole.iter().enumerate() {
            let (offset, inner) = self.field(&kind, name)?;
            let last = step + 1 == whole.len();
            let fits = match &inner {
                Kind::Dict(..) => last,
                Kind::Boxed(_) | Kind::Struct(_) => !last,
                _ => false,
            };
            if !fits {
                return Ok(None);
            }
            steps.push((offset, matches!(inner, Kind::Boxed(_))));
            kind = inner;
        }
        let (cell, _) = self.operand(&Operand::Local(handle))?;
        let mut place = self.offset(&cell, FIELDS_OFFSET);
        let mut all = "true".to_owned();
        let mut and = |emit: &mut Self, alone: String| {
            let both = emit.temp();
            emit.line(&format!("{both} = and i1 {all}, {alone}"));
            all = both;
        };
        let last = steps.len().saturating_sub(1);
        for (step, (offset, boxed)) in steps.into_iter().enumerate() {
            place = self.offset(&place, offset);
            if step < last && boxed {
                let block = self.temp();
                self.line(&format!("{block} = load ptr, ptr {place}"));
                let alone = self.alone(&block);
                and(self, alone);
                place = self.offset(&block, FIELDS_OFFSET);
            }
        }
        let dict = self.temp();
        self.line(&format!("{dict} = load ptr, ptr {place}"));
        let alone = self.alone(&dict);
        and(self, alone);
        Ok(Some((place, all)))
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
        assert!(pay.contains("call i64 @fennec_lock_begin("), "{pay}");
        for call in ["save_text", "save_ref", "kept"] {
            assert!(pay.contains(&format!("call void @fennec_lock_{call}(")), "{call}: {pay}");
        }
        // The balance, a number, is saved without a call while there's room.
        assert!(pay.contains("call void @fen_lock_save("), "{pay}");
        // The record's slot starts at 0, before any lock's begin stores the record in it.
        let lines: Vec<&str> = pay.lines().map(str::trim).collect();
        let zeroed = lines.windows(2).any(|pair| {
            pair[0]
                .strip_suffix(" = alloca i64")
                .is_some_and(|slot| pair[1] == format!("store i64 0, ptr {slot}"))
        });
        assert!(zeroed, "{pay}");
        assert!(text.contains("define internal void @fen_lock_save("), "{text}");
        let main =
            text.split("define").find(|function| function.contains("@\"f_main\"(")).unwrap_or_default();
        assert!(!main.contains("@fennec_lock_begin("), "{main}");
        Ok(())
    }

    /// A function with a lock to write leaves on every failure through `prop`, which puts back
    /// what the lock changed, word by word for its own cell, before the failure goes on.
    #[test]
    fn a_crash_in_a_lock_to_write_goes_through_the_undo() -> Result<(), String> {
        let source = "\
struct Tally:
    names: list[str]
    count = 0

def hit(tally: shared Tally):
    lock tally as ref held:
        held.count += 1
        held.names.append(\"x\")
        empty: list[int] = []
        held.count += empty[3]

def main(stdout: Stdout):
    shared tally = Tally(names=[])
    hit(tally)
";
        let text = texts(source)?;
        let hit = text.split("define").find(|function| function.contains("@\"f_hit\"(")).unwrap_or_default();
        let prop = hit.split("\nprop:\n").nth(1).unwrap_or_default();
        assert!(prop.contains("call void @fennec_lock_undo()"), "{hit}");
        assert!(prop.contains("call i64 @fennec_lock_undo_word("), "{hit}");
        assert!(hit.contains("br label %prop"), "{hit}");
        Ok(())
    }
}
