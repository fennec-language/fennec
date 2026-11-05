//! A `shared` value's lock: the word after its cell's fields. Zero when it's free, the top bit for
//! a writer, and otherwise how many are reading.
//!
//! With only one thread, a lock that's held is held by this thread further up, so plain loads and
//! stores do. With threads the word is taken with atomics, and the runtime keeps the locks each
//! thread holds, to tell a lock another thread holds, which is waited for, from one this thread
//! does, which would wait forever and ends the program.

use fennec_ir::Operand;
use fennec_layout::{Layout, Layouts, Ty};
use fennec_syntax::Span;

use crate::emit::Emit;
use crate::kinds::Kind;
use crate::lists::FIELDS_OFFSET;

/// What a `with` that takes one `shared` value under two names says, in every engine.
const TAKEN_TWICE: &str = "this `with` takes the same `shared` value twice";

/// What taking the lock of a `shared` value this thread holds already says, in every engine.
const TAKEN_AGAIN: &str = "this takes the lock of a `shared` value that's already held";

/// The word's top bit: a writer holds the lock.
const WRITER: i64 = i64::MIN;

impl Emit<'_, '_, '_> {
    /// Where a cell's lock is: the word after its fields. On threads, what a cell holds has to be
    /// readable by every thread, which only plain numbers are until the runtime publishes blocks.
    fn lock_word(&mut self, handle: &Operand) -> Result<String, String> {
        let (value, kind) = self.operand(handle)?;
        let Kind::Boxed(id) = kind else { return Err(format!("it locks a {kind:?}")) };
        let layout = self.layout_of(id)?;
        let size = layout.size;
        if self.module.threads && !plain_on_threads(self.module.layouts, layout) {
            let name = layout.name.trim_end_matches('\'').to_owned();
            return Err(format!(
                "`{name}` holds something other than numbers, which this backend doesn't take on threads yet"
            ));
        }
        Ok(self.offset(&value, FIELDS_OFFSET + size - 8))
    }

    /// Ends the program if `taken`, an `i1`, is true, and carries on otherwise.
    fn stop_if(&mut self, taken: &str, message: &str, span: Span) {
        let (stop, carry_on) = (self.label("lock_stops"), self.label("lock_ok"));
        self.line(&format!("br i1 {taken}, label %{stop}, label %{carry_on}, !prof !8"));
        self.start_block(&stop);
        self.stop(message, span);
        self.start_block(&carry_on);
    }

    pub(crate) fn lock(&mut self, handle: &Operand, write: bool, span: Span) -> Result<(), String> {
        let word = self.lock_word(handle)?;
        self.take_word(&word, if write { "true" } else { "false" }, span);
        Ok(())
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
        self.declare("declare i64 @fennec_lock_note(ptr) nounwind");
        self.declare("declare void @fennec_lock_wait() nounwind");
        let (noted, again) = (self.temp(), self.temp());
        self.line(&format!("{noted} = call i64 @fennec_lock_note(ptr {word})"));
        self.line(&format!("{again} = icmp ne i64 {noted}, 0"));
        self.stop_if(&again, TAKEN_AGAIN, span);
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
        self.line("call void @fennec_lock_wait()");
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

    pub(crate) fn unlock(&mut self, handle: &Operand) -> Result<(), String> {
        let word = self.lock_word(handle)?;
        if !self.module.threads {
            self.line(&format!("store i64 0, ptr {word}"));
            return Ok(());
        }
        self.declare("declare void @fennec_lock_forget(ptr) nounwind");
        let (old, writer) = (self.temp(), self.temp());
        // Only this thread can have set the top bit, if it's set, so it's safe to look first.
        self.line(&format!("{old} = load atomic i64, ptr {word} monotonic, align 8"));
        self.line(&format!("{writer} = icmp slt i64 {old}, 0"));
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
        self.line(&format!("call void @fennec_lock_forget(ptr {word})"));
        Ok(())
    }

    /// Taking several `shared` values at once: the same one twice ends the program, and otherwise
    /// they're taken in order of address, so that no two `with`s each wait for the other.
    pub(crate) fn lock_all(&mut self, handles: &[(Operand, bool)], span: Span) -> Result<(), String> {
        let mut held = Vec::with_capacity(handles.len());
        for (handle, write) in handles {
            held.push((self.lock_word(handle)?, write.to_string()));
        }
        for (at, (word, _)) in held.iter().enumerate() {
            for (earlier, _) in held.iter().take(at) {
                let same = self.temp();
                self.line(&format!("{same} = icmp eq ptr {earlier}, {word}"));
                self.stop_if(&same, TAKEN_TWICE, span);
            }
        }
        if self.module.threads {
            self.sort_by_address(&mut held);
        }
        for (word, write) in &held {
            self.take_word(word, write, span);
        }
        Ok(())
    }

    /// Puts the words in order of address, with what says each one's written, as selects.
    fn sort_by_address(&mut self, held: &mut [(String, String)]) {
        for first in 0..held.len() {
            for second in first + 1..held.len() {
                let (Some((a, a_write)), Some((b, b_write))) =
                    (held.get(first).cloned(), held.get(second).cloned())
                else {
                    continue;
                };
                let (swap, low, high, low_write, high_write) =
                    (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
                self.line(&format!("{swap} = icmp ugt ptr {a}, {b}"));
                self.line(&format!("{low} = select i1 {swap}, ptr {b}, ptr {a}"));
                self.line(&format!("{high} = select i1 {swap}, ptr {a}, ptr {b}"));
                self.line(&format!("{low_write} = select i1 {swap}, i1 {b_write}, i1 {a_write}"));
                self.line(&format!("{high_write} = select i1 {swap}, i1 {a_write}, i1 {b_write}"));
                if let Some(slot) = held.get_mut(first) {
                    *slot = (low, low_write);
                }
                if let Some(slot) = held.get_mut(second) {
                    *slot = (high, high_write);
                }
            }
        }
    }
}

/// Whether a cell, or a struct in place in one, holds only numbers and bools.
fn plain_on_threads(layouts: &Layouts, layout: &Layout) -> bool {
    layout.fields.iter().all(|field| match field.ty {
        Ty::Int | Ty::Float | Ty::Bool => true,
        Ty::Struct(id) => layouts
            .get(id)
            .is_some_and(|inner| !inner.boxed && inner.cases.is_none() && plain_on_threads(layouts, inner)),
        _ => false,
    })
}
