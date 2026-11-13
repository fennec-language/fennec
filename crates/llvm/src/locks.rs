//! A `shared` value's lock: the word after its cell's fields. Zero when it's free, the top bit for
//! a writer, and otherwise how many are reading.
//!
//! With only one thread, a lock that's held is held by this thread further up, so plain loads and
//! stores do. With threads the word is taken with atomics, and the runtime keeps the locks each
//! thread holds, to tell a lock another thread holds, which is waited for, from one this thread
//! does, which would wait forever and ends the program. Holding the word, a thread also takes the
//! value's table of blocks from the runtime, and what crosses the lock's edge is copied.

use std::fmt::Write as _;

use fennec_ir::{LocalId, Operand};
use fennec_layout::{Layout, Layouts, Ty};
use fennec_syntax::Span;

use crate::emit::Emit;
use crate::headers::HEADER;
use crate::kinds::Kind;
use crate::lists::FIELDS_OFFSET;

/// What a `with` that takes one `shared` value under two names says, in every engine.
const TAKEN_TWICE: &str = "this `lock` takes the same `shared` value twice";

/// What taking the lock of a `shared` value this thread holds already says, in every engine.
const TAKEN_AGAIN: &str = "this takes the lock of a `shared` value that's already held";

/// The word's top bit: a writer holds the lock.
const WRITER: i64 = i64::MIN;

/// Which way a value crosses a lock's edge. Coming out, it goes behind the write locks being left.
#[derive(Clone, Copy)]
pub(crate) enum Way {
    In,
    Out(i64),
}

/// A lock to take or give back: its word, its cell, an `i1` saying whether it's to write, and one
/// saying whether its value has no table, holding only numbers.
type Held = (String, String, String, String);

impl Emit<'_, '_, '_> {
    /// Where a cell's lock is, the word after its fields, the cell, and whether it holds only numbers.
    fn lock_word(&mut self, handle: &Operand) -> Result<(String, String, bool), String> {
        let (value, kind) = self.operand(handle)?;
        let Kind::Boxed(id) = kind else { return Err(format!("it locks a {kind:?}")) };
        let layout = self.layout_of(id)?;
        let (size, plain) = (layout.size, plain_on_threads(self.module.layouts, layout));
        Ok((self.offset(&value, FIELDS_OFFSET + size - 8), value, plain))
    }

    /// Runs `body` unless `plain`, an `i1`, says a value holds only numbers.
    fn unless_plain(&mut self, plain: &str, body: impl FnOnce(&mut Self)) {
        match plain {
            "true" => {}
            "false" => body(self),
            _ => {
                let (has_table, done) = (self.label("has_table"), self.label("table_done"));
                self.line(&format!("br i1 {plain}, label %{done}, label %{has_table}"));
                self.start_block(&has_table);
                body(self);
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
            }
        }
    }

    /// Ends the program if `taken`, an `i1`, is true, and carries on otherwise.
    pub(crate) fn stop_if(&mut self, taken: &str, message: &str, span: Span) {
        let (stop, carry_on) = (self.label("lock_stops"), self.label("lock_ok"));
        self.line(&format!("br i1 {taken}, label %{stop}, label %{carry_on}, !prof !8"));
        self.start_block(&stop);
        self.stop(message, span);
        self.start_block(&carry_on);
    }

    pub(crate) fn lock(&mut self, handle: &Operand, write: bool, span: Span) -> Result<(), String> {
        let (word, cell, plain) = self.lock_word(handle)?;
        let write = if write { "true" } else { "false" };
        self.take_and_enter((&word, &cell), write, &plain.to_string(), span);
        if self.saves_changes(handle) {
            self.rollback_begin(handle, &cell);
        } else if self.read_lock_given_back(handle) {
            self.note_read_lock(handle, true);
        }
        Ok(())
    }

    /// Takes the lock at `word`, the word of `cell`, and enters the cell, to write if `write`, an
    /// `i1`, is true. While the run is recorded or replayed, the runtime is asked first whether
    /// it's this task's turn, and told once it's taken, so a replay takes locks in the order the
    /// recording did.
    fn take_and_enter(&mut self, (word, cell): (&str, &str), write: &str, plain: &str, span: Span) {
        let replaying = self.module.threads.then(|| self.replaying());
        if let Some(replaying) = &replaying {
            self.if_replaying(replaying, "fennec_replay_lock", cell, write);
        }
        self.take_word(word, write, span);
        self.entered(cell, write, plain);
        if let Some(replaying) = &replaying {
            self.if_replaying(replaying, "fennec_replay_locked", cell, write);
        }
    }

    /// Whether this run is recorded or replayed, an `i1`: the word after the lock this thread
    /// holds, beside its failure flag.
    fn replaying(&mut self) -> String {
        let flag = self.failure_flag();
        let at = self.offset(&flag, 16);
        let (word, on) = (self.temp(), self.temp());
        self.line(&format!("{word} = load i64, ptr {at}"));
        self.line(&format!("{on} = icmp ne i64 {word}, 0"));
        on
    }

    /// Calls the runtime's `call` with the cell and whether it's to write, when `replaying`.
    fn if_replaying(&mut self, replaying: &str, call: &str, cell: &str, write: &str) {
        self.declare(&format!("declare void @{call}(ptr, i64) nounwind"));
        let (asks, done) = (self.label("replay_turn"), self.label("replay_done"));
        self.line(&format!("br i1 {replaying}, label %{asks}, label %{done}, !prof !0"));
        self.start_block(&asks);
        let wide = self.temp();
        self.line(&format!("{wide} = zext i1 {write} to i64"));
        self.line(&format!("call void @{call}(ptr {cell}, i64 {wide})"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// Takes the lock at `word`, to write if `write`, an `i1`, is true.
    fn take_word(&mut self, word: &str, write: &str, span: Span) {
        if !self.module.threads {
            let (held, taken) = (self.temp(), self.temp());
            self.line(&format!("{held} = load i64, ptr {word}"));
            self.line(&format!("{taken} = icmp ne i64 {held}, 0"));
            self.stop_if(&taken, TAKEN_AGAIN, span);
            self.line(&format!("store i64 1, ptr {word}"));
            return;
        }
        self.declare("declare void @fennec_shared_wait() nounwind");
        self.note_taking(word, span);
        let (look, wait, try_it, done) =
            (self.label("lock_look"), self.label("lock_wait"), self.label("lock_try"), self.label("locked"));
        self.line(&format!("br label %{look}"));
        self.start_block(&look);
        let (old, any, writer, blocked) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{old} = load atomic i64, ptr {word} monotonic, align 8"));
        self.line(&format!("{any} = icmp ne i64 {old}, 0"));
        self.line(&format!("{writer} = icmp slt i64 {old}, 0"));
        self.line(&format!("{blocked} = select i1 {write}, i1 {any}, i1 {writer}"));
        self.line(&format!("br i1 {blocked}, label %{wait}, label %{try_it}"));
        self.start_block(&wait);
        self.line("call void @fennec_shared_wait()");
        self.line(&format!("br label %{look}"));
        self.start_block(&try_it);
        let (reader, want, swapped, took) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{reader} = add i64 {old}, 1"));
        self.line(&format!("{want} = select i1 {write}, i64 {WRITER}, i64 {reader}"));
        self.line(&format!("{swapped} = cmpxchg ptr {word}, i64 {old}, i64 {want} acquire monotonic"));
        self.line(&format!("{took} = extractvalue {{ i64, i1 }} {swapped}, 1"));
        self.line(&format!("br i1 {took}, label %{done}, label %{look}"));
        self.start_block(&done);
    }

    /// Where a thread remembers the one lock it holds: beside its failure flag, so the runtime can
    /// tell when a body that failed leaves it held.
    fn lock_held_place(&mut self) -> String {
        let flag = self.failure_flag();
        self.offset(&flag, 8)
    }

    /// Stops if this thread holds the lock already. A thread remembers one lock itself, so one at a
    /// time costs no call, and the runtime remembers any held beside it.
    fn note_taking(&mut self, word: &str, span: Span) {
        let held_at = self.lock_held_place();
        let (first, same, empty) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{first} = load ptr, ptr {held_at}"));
        self.line(&format!("{same} = icmp eq ptr {first}, {word}"));
        self.stop_if(&same, TAKEN_AGAIN, span);
        self.line(&format!("{empty} = icmp eq ptr {first}, null"));
        let (claim, another, noted) =
            (self.label("lock_first"), self.label("lock_another"), self.label("lock_noted"));
        self.line(&format!("br i1 {empty}, label %{claim}, label %{another}, !prof !8"));
        self.start_block(&claim);
        self.line(&format!("store ptr {word}, ptr {held_at}"));
        self.line(&format!("br label %{noted}"));
        self.start_block(&another);
        self.declare("declare i64 @fennec_lock_note(ptr) nounwind");
        let (asked, again) = (self.temp(), self.temp());
        self.line(&format!("{asked} = call i64 @fennec_lock_note(ptr {word})"));
        self.line(&format!("{again} = icmp ne i64 {asked}, 0"));
        self.stop_if(&again, TAKEN_AGAIN, span);
        self.line(&format!("br label %{noted}"));
        self.start_block(&noted);
    }

    /// Forgets that this thread holds a lock.
    fn note_given_back(&mut self, word: &str) {
        let held_at = self.lock_held_place();
        let (first, same) = (self.temp(), self.temp());
        self.line(&format!("{first} = load ptr, ptr {held_at}"));
        self.line(&format!("{same} = icmp eq ptr {first}, {word}"));
        let (mine, other, done) =
            (self.label("unlock_first"), self.label("unlock_other"), self.label("unlock_noted"));
        self.line(&format!("br i1 {same}, label %{mine}, label %{other}, !prof !8"));
        self.start_block(&mine);
        self.line(&format!("store ptr null, ptr {held_at}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&other);
        self.declare("declare void @fennec_lock_forget(ptr) nounwind");
        self.line(&format!("call void @fennec_lock_forget(ptr {word})"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// With the word held, the thread takes the cell's table and goes by what the runtime says. A
    /// value holding only numbers has nothing in its table that anything could reach, so that's all
    /// its lock is: what a body makes stays the thread's own.
    fn entered(&mut self, cell: &str, write: &str, plain: &str) {
        if !self.module.threads {
            return;
        }
        self.unless_plain(plain, |emit| {
            emit.declare("declare i64 @fennec_shared_enter(ptr, i64) nounwind");
            emit.declare(crate::parallel::thread_global());
            let (wide, me) = (emit.temp(), emit.temp());
            emit.line(&format!("{wide} = zext i1 {write} to i64"));
            emit.line(&format!("{me} = call i64 @fennec_shared_enter(ptr {cell}, i64 {wide})"));
            emit.leave_if_failed();
            emit.line(&format!("store i64 {me}, ptr @fennec_thread"));
        });
    }

    pub(crate) fn unlock(&mut self, handle: &Operand) -> Result<(), String> {
        let (word, cell, plain) = self.lock_word(handle)?;
        // A `lock ... as ref` block ending normally keeps what it changed.
        if self.saves_changes(handle) && !self.take_rolled_back(handle) {
            self.rollback_kept(handle);
        } else if self.read_lock_given_back(handle) {
            self.note_read_lock(handle, false);
        }
        self.give_back_word(&word, &cell, &plain.to_string(), false);
        Ok(())
    }

    /// Gives a lock's word back from where a crash leaves the function, with its failure already
    /// set, so leaving the value's table can't send it back there.
    pub(crate) fn give_back_on_crash(&mut self, handle: &Operand) -> Result<(), String> {
        let (word, cell, plain) = self.lock_word(handle)?;
        self.give_back_word(&word, &cell, &plain.to_string(), true);
        Ok(())
    }

    fn give_back_word(&mut self, word: &str, cell: &str, plain: &str, on_crash: bool) {
        if !self.module.threads {
            self.line(&format!("store i64 0, ptr {word}"));
            return;
        }
        let (old, writer) = (self.temp(), self.temp());
        // Only this thread can have set the top bit, if it's set, so it's safe to look first.
        self.line(&format!("{old} = load atomic i64, ptr {word} monotonic, align 8"));
        self.line(&format!("{writer} = icmp slt i64 {old}, 0"));
        self.unless_plain(plain, |emit| {
            emit.declare("declare i64 @fennec_shared_leave(ptr, i64) nounwind");
            emit.declare(crate::parallel::thread_global());
            let (wide, me) = (emit.temp(), emit.temp());
            emit.line(&format!("{wide} = zext i1 {writer} to i64"));
            emit.line(&format!("{me} = call i64 @fennec_shared_leave(ptr {cell}, i64 {wide})"));
            if !on_crash {
                emit.leave_if_failed();
            }
            emit.line(&format!("store i64 {me}, ptr @fennec_thread"));
        });
        let (free, read, done) =
            (self.label("unlock_free"), self.label("unlock_read"), self.label("unlocked"));
        self.line(&format!("br i1 {writer}, label %{free}, label %{read}"));
        self.start_block(&free);
        self.line(&format!("store atomic i64 0, ptr {word} release, align 8"));
        self.line(&format!("br label %{done}"));
        self.start_block(&read);
        let gone = self.temp();
        self.line(&format!("{gone} = atomicrmw sub ptr {word}, i64 1 release"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        self.note_given_back(word);
    }

    /// Taking several `shared` values at once: the same one twice ends the program, and otherwise
    /// they're taken in order of address, so that no two `with`s each wait for the other.
    pub(crate) fn lock_all(&mut self, handles: &[(Operand, bool)], span: Span) -> Result<(), String> {
        let mut held = Vec::with_capacity(handles.len());
        for (handle, write) in handles {
            let (word, cell, plain) = self.lock_word(handle)?;
            held.push((word, cell, write.to_string(), plain.to_string()));
        }
        for (at, (word, ..)) in held.iter().enumerate() {
            for (earlier, ..) in held.iter().take(at) {
                let same = self.temp();
                self.line(&format!("{same} = icmp eq ptr {earlier}, {word}"));
                self.stop_if(&same, TAKEN_TWICE, span);
            }
        }
        if self.module.threads {
            self.sort_by_address(&mut held);
        }
        for (word, cell, write, plain) in &held {
            self.take_and_enter((word, cell), write, plain, span);
        }
        for (handle, _) in handles {
            if self.saves_changes(handle) {
                let (cell, _) = self.operand(handle)?;
                self.rollback_begin(handle, &cell);
            } else if self.read_lock_given_back(handle) {
                self.note_read_lock(handle, true);
            }
        }
        Ok(())
    }

    /// Giving back several `shared` values' locks, in the opposite order from taking them, as the
    /// tables a thread holds are a stack.
    pub(crate) fn unlock_all(&mut self, handles: &[Operand]) -> Result<(), String> {
        // Each `lock ... as ref` among them ending normally keeps what it changed.
        for handle in handles.iter().rev() {
            if self.saves_changes(handle) && !self.take_rolled_back(handle) {
                self.rollback_kept(handle);
            } else if self.read_lock_given_back(handle) {
                self.note_read_lock(handle, false);
            }
        }
        let mut held = Vec::with_capacity(handles.len());
        for handle in handles {
            let (word, cell, plain) = self.lock_word(handle)?;
            held.push((word, cell, "false".to_owned(), plain.to_string()));
        }
        if self.module.threads {
            self.sort_by_address(&mut held);
            for (word, cell, _, plain) in &held {
                self.unless_plain(plain, |emit| emit.settle(word, cell));
            }
        }
        for (word, cell, _, plain) in held.iter().rev() {
            self.give_back_word(word, cell, plain, false);
        }
        Ok(())
    }

    /// With several held, what's made in the body is in whichever table's on top, and one value can
    /// be given what's in another's, so each one held to write is given its own copy.
    fn settle(&mut self, word: &str, cell: &str) {
        self.declare("declare void @fennec_shared_settle(ptr) nounwind");
        let (old, writer) = (self.temp(), self.temp());
        self.line(&format!("{old} = load atomic i64, ptr {word} monotonic, align 8"));
        self.line(&format!("{writer} = icmp slt i64 {old}, 0"));
        let (settle, done) = (self.label("settle"), self.label("settled"));
        self.line(&format!("br i1 {writer}, label %{settle}, label %{done}"));
        self.start_block(&settle);
        self.line(&format!("call void @fennec_shared_settle(ptr {cell})"));
        self.leave_if_failed();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// Puts locks in order of address, as selects, since it's only known when they're taken.
    fn sort_by_address(&mut self, held: &mut [Held]) {
        for first in 0..held.len() {
            for second in first + 1..held.len() {
                let (Some(a), Some(b)) = (held.get(first).cloned(), held.get(second).cloned()) else {
                    continue;
                };
                let swap = self.temp();
                self.line(&format!("{swap} = icmp ugt ptr {}, {}", a.0, b.0));
                let low = (
                    self.pick(&swap, "ptr", &b.0, &a.0),
                    self.pick(&swap, "ptr", &b.1, &a.1),
                    self.pick(&swap, "i1", &b.2, &a.2),
                    self.pick(&swap, "i1", &b.3, &a.3),
                );
                let high = (
                    self.pick(&swap, "ptr", &a.0, &b.0),
                    self.pick(&swap, "ptr", &a.1, &b.1),
                    self.pick(&swap, "i1", &a.2, &b.2),
                    self.pick(&swap, "i1", &a.3, &b.3),
                );
                if let Some(slot) = held.get_mut(first) {
                    *slot = low;
                }
                if let Some(slot) = held.get_mut(second) {
                    *slot = high;
                }
            }
        }
    }

    fn pick(&mut self, swap: &str, ty: &str, one: &str, other: &str) -> String {
        let picked = self.temp();
        self.line(&format!("{picked} = select i1 {swap}, {ty} {one}, {ty} {other}"));
        picked
    }

    /// `dest = value`, copied across a lock's edge, with owners of its own.
    pub(crate) fn across_lock(&mut self, dest: LocalId, value: &Operand, way: Way) -> Result<(), String> {
        let (held, kind) = self.operand(value)?;
        if kind.in_memory() {
            let (at, text) = (self.label("%crossed"), self.text_of(&kind));
            let _ = writeln!(self.allocas, "  {at} = alloca {text}");
            self.cross_into(&held, &kind, way, &at)?;
            return self.assign_memory_owned(dest, &at);
        }
        let made = self.crossed(&held, &kind, way)?;
        self.assign(dest, &made)
    }

    /// A value copied across a lock's edge, as it's passed around, with owners of its own.
    fn crossed(&mut self, value: &str, kind: &Kind, way: Way) -> Result<String, String> {
        match kind {
            Kind::Int | Kind::Float | Kind::Bool | Kind::U64 | Kind::F32 | Kind::Unit | Kind::Cap => {
                Ok(value.to_owned())
            }
            Kind::Str | Kind::MaybeText if self.headers() => Ok(self.crossed_text(value, way)),
            Kind::Boxed(_) if self.is_cell(kind) => {
                self.retain(value);
                Ok(value.to_owned())
            }
            // Any other block is copied by the runtime, by its shape, as a boxed value's is.
            Kind::Str
            | Kind::TextBlock
            | Kind::List(_)
            | Kind::Dict(..)
            | Kind::Boxed(_)
            | Kind::Bytes
            | Kind::Closure(..)
            | Kind::Gen => Ok(self.cross_block(value, way)),
            Kind::Maybe(inner) if !self.is_cell(inner) => Ok(self.cross_block(value, way)),
            Kind::Maybe(_) => {
                let place = self.temp_slot("null");
                let (none, some, done) = (self.temp(), self.label("cross_some"), self.label("crossed_some"));
                self.line(&format!("{none} = icmp eq ptr {value}, null"));
                self.line(&format!("br i1 {none}, label %{done}, label %{some}"));
                self.start_block(&some);
                self.retain(value);
                self.line(&format!("store ptr {value}, ptr {place}"));
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                let held = self.temp();
                self.line(&format!("{held} = load ptr, ptr {place}"));
                Ok(held)
            }
            other => {
                Err(format!("a {other:?} crosses a `shared` value's lock, which isn't taken on threads yet"))
            }
        }
    }

    /// A string held as a header: an inline one is its own copy.
    fn crossed_text(&mut self, value: &str, way: Way) -> String {
        let in_block = self.header_in_block(value);
        let (block, inline, done) =
            (self.label("cross_text"), self.label("cross_inline"), self.label("crossed_text"));
        let place = self.place_for_text();
        self.line(&format!("store {HEADER} {value}, ptr {place}"));
        self.line(&format!("br i1 {in_block}, label %{block}, label %{inline}"));
        self.start_block(&block);
        let first = self.header_word(value, 0);
        let held = self.temp();
        self.line(&format!("{held} = inttoptr i64 {first} to ptr"));
        let copy = self.cross_block(&held, way);
        let header = self.text_from_block(&copy);
        self.line(&format!("store {HEADER} {header}, ptr {place}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&inline);
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let header = self.temp();
        self.line(&format!("{header} = load {HEADER}, ptr {place}"));
        header
    }

    /// One block's copy from the runtime, unless there's nothing there.
    fn cross_block(&mut self, block: &str, way: Way) -> String {
        let place = self.temp_slot("null");
        let (none, some, done) = (self.temp(), self.label("cross_block"), self.label("crossed_block"));
        self.line(&format!("{none} = icmp eq ptr {block}, null"));
        self.line(&format!("br i1 {none}, label %{done}, label %{some}"));
        self.start_block(&some);
        let copy = self.temp();
        match way {
            Way::In => {
                self.declare("declare ptr @fennec_shared_in(ptr) nounwind");
                self.line(&format!("{copy} = call ptr @fennec_shared_in(ptr {block})"));
            }
            Way::Out(held) => {
                self.declare("declare ptr @fennec_shared_out(ptr, i64) nounwind");
                self.line(&format!("{copy} = call ptr @fennec_shared_out(ptr {block}, i64 {held})"));
            }
        }
        self.leave_if_failed();
        self.line(&format!("store ptr {copy}, ptr {place}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let held = self.temp();
        self.line(&format!("{held} = load ptr, ptr {place}"));
        held
    }

    /// A struct or enum in place, copied at `at`, each thing it holds across the edge.
    fn cross_into(&mut self, value: &str, kind: &Kind, way: Way, at: &str) -> Result<(), String> {
        let text = self.text_of(kind);
        let whole = self.temp();
        self.line(&format!("{whole} = load {text}, ptr {value}"));
        self.line(&format!("store {text} {whole}, ptr {at}"));
        let layouts = self.module.layouts;
        match kind {
            Kind::Struct(id) => {
                for field in &layouts.get(*id).ok_or("no such layout")?.fields {
                    let held =
                        Kind::from_ty(layouts, field.ty).ok_or("a field this backend doesn't take yet")?;
                    self.cross_field((value, at), field.offset, &held, way)?;
                }
            }
            Kind::Enum(id) => {
                let cases = layouts
                    .get(*id)
                    .and_then(|layout| layout.cases.as_ref())
                    .ok_or("an enum with no cases")?;
                let tag = self.temp();
                self.line(&format!("{tag} = load i64, ptr {value}"));
                let done = self.label("crossed_enum");
                for (number, case) in cases.iter().enumerate() {
                    let (same, this, next) = (self.temp(), self.label("cross_case"), self.label("cross_not"));
                    self.line(&format!("{same} = icmp eq i64 {tag}, {number}"));
                    self.line(&format!("br i1 {same}, label %{this}, label %{next}"));
                    self.start_block(&this);
                    for field in &case.fields {
                        let held = Kind::from_ty(layouts, field.ty)
                            .ok_or("a field this backend doesn't take yet")?;
                        self.cross_field((value, at), field.offset, &held, way)?;
                    }
                    self.line(&format!("br label %{done}"));
                    self.start_block(&next);
                }
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
            }
            other => {
                return Err(format!(
                    "a {other:?} crosses a `shared` value's lock, which isn't taken on threads yet"
                ));
            }
        }
        Ok(())
    }

    fn cross_field(
        &mut self,
        (value, at): (&str, &str),
        offset: u32,
        held: &Kind,
        way: Way,
    ) -> Result<(), String> {
        let (from, to) = (self.offset(value, offset), self.offset(at, offset));
        if held.in_memory() {
            return self.cross_into(&from, held, way, &to);
        }
        if !held.counted() {
            return Ok(());
        }
        let inside = self.load_field(held, &from, "");
        let copy = self.crossed(&inside, held, way)?;
        self.store_field(held, &copy, &to, "");
        Ok(())
    }

    /// Lets go of what a local holds, leaving nothing there.
    pub(crate) fn drop_local(&mut self, local: LocalId) -> Result<(), String> {
        let kind = self.kind_of(local)?.clone();
        if !kind.counted() && !self.holds_inline(&kind) {
            return Ok(());
        }
        let (slot, text) = (self.slot(local), self.text_of(&kind));
        if kind.in_memory() {
            self.release_held(local)?;
        } else if self.owns(local) {
            let old = self.temp();
            self.line(&format!("{old} = load {text}, ptr {slot}"));
            self.release_kind(&old, &kind);
        }
        self.line(&format!("store {text} zeroinitializer, ptr {slot}"));
        Ok(())
    }
}

/// Whether a `shared` value, or a struct in place in one, holds only numbers and bools.
fn plain_on_threads(layouts: &Layouts, layout: &Layout) -> bool {
    layout.fields.iter().all(|field| match field.ty {
        Ty::Int | Ty::Float | Ty::Bool => true,
        Ty::Struct(id) => layouts
            .get(id)
            .is_some_and(|inner| !inner.boxed && inner.cases.is_none() && plain_on_threads(layouts, inner)),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use fennec_ir::InstKind;

    /// `main`'s text, and the literals the program uses, for `source` with its second lock moved
    /// into its first: right after the first is taken, so it's taken while the first is held.
    fn nested(source: &str) -> Result<(String, Vec<String>), String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        // As a build lowers it: `parallel`'s work lifted out, so the program runs on threads.
        let options =
            fennec_ir::Options { outline_parallel: true, stop_on_failed_main: true, serve_in_order: false };
        let mut program = fennec_ir::lower_with(&module, &checked, options);
        let main = program.functions.iter_mut().find(|function| function.name == "main").ok_or("no main")?;
        let locks: Vec<(usize, usize)> = main
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(at, block)| {
                block.insts.iter().enumerate().filter_map(move |(index, inst)| {
                    matches!(inst.kind, InstKind::Lock { .. }).then_some((at, index))
                })
            })
            .collect();
        let [(first_block, first), (second_block, second)] = locks[..] else {
            return Err(format!("{} locks, not two", locks.len()));
        };
        let taken = main.blocks[second_block].insts.remove(second);
        let after = if (first_block, first) < (second_block, second) { first + 1 } else { first };
        main.blocks[first_block].insts.insert(after, taken);
        let (units, shared, _) = crate::emit::emit(&program)?;
        let text =
            units.into_iter().find(|unit| unit.function == "main").map(|unit| unit.text).ok_or("no main")?;
        Ok((text, shared.literals.into_values().collect()))
    }

    /// A lock this thread holds, taken again, ends the program, as it would wait forever. The
    /// checker refuses every program that would get there, a closure passed to a call among them
    /// (#1261), so nothing built from source reaches it: here the IR is built to, a lock of one
    /// value taken inside another of it. The lock taken second still checks the lock's word and
    /// stops, without threads; with them it also checks the lock it remembers and asks the
    /// runtime about any others.
    #[test]
    fn a_lock_taken_inside_itself_stops_the_program() -> Result<(), String> {
        let program = "struct Box:\n    total = 1\n\ndef main(stdout: Stdout):\n    shared one = Box()\n    lock one as held:\n        stdout.print(held.total)\n    lock one as again:\n        stdout.print(again.total)\n";
        let (text, literals) = nested(program)?;
        assert!(literals.iter().any(|literal| literal.contains(super::TAKEN_AGAIN)), "{literals:?}");
        assert_eq!(text.matches("label %lock_stops").count(), 2, "{text}");
        let threads = format!("{program}    stdout.print(sum(n for n in parallel([1, 2])))\n");
        let (text, literals) = nested(&threads)?;
        assert!(literals.iter().any(|literal| literal.contains(super::TAKEN_AGAIN)), "{literals:?}");
        assert!(text.matches("label %lock_stops").count() >= 4, "{text}");
        assert!(text.contains("call i64 @fennec_lock_note("), "{text}");
        Ok(())
    }
}
