//! Generators, as the IR builds them: a block holding where it stopped, its item and the locals
//! it keeps, and a function that runs it on to its next item, whose address is in the block.
//! Every generator's block starts the same way, so a loop reads one without knowing which it is.

use fennec_check::Type;
use fennec_ir::{GeneratorDef, LocalId, Operand, slot};
use fennec_rt::embed::layout::{FIELDS, GEN_FAILURE, GEN_ITEM, GEN_PC, GEN_RESUME, GEN_STARTED};
use fennec_syntax::Span;

use crate::emit::{Emit, symbol};
use crate::kinds::Kind;

/// Where a word of the header is, from the block's start.
fn header(offset: i32) -> u32 {
    offset.unsigned_abs()
}

impl<'p> Emit<'_, '_, 'p> {
    /// The generator this function runs or closes, when `generator` is that one: its block
    /// comes first. Any other generator it reads is only known by its header.
    fn own_generator(&self, generator: &Operand) -> Option<&'p GeneratorDef> {
        if *generator != Operand::Local(LocalId(0)) {
            return None;
        }
        let def = self.module.generator(&self.function.name)?;
        (def.resume == self.function.name || def.abandon.as_deref() == Some(self.function.name.as_str()))
            .then_some(def)
    }

    /// How a slot is held, and where it is from the block's start. The header's slots are the
    /// same in every generator; the locals it keeps are only known to its own functions.
    fn slot_place(
        &self,
        def: Option<&GeneratorDef>,
        generator: &Operand,
        number: usize,
    ) -> Result<(Kind, u32), String> {
        match number {
            slot::PC => return Ok((Kind::Int, header(GEN_PC))),
            slot::STARTED => return Ok((Kind::Bool, header(GEN_STARTED))),
            // What it failed with is a block of its own, as a slot is one word in every generator.
            slot::FAILURE => return Ok((Kind::Int, header(GEN_FAILURE))),
            slot::ITEM => {
                let item = if let Some(def) = def {
                    def.item.clone()
                } else {
                    let Operand::Local(local) = generator else {
                        return Err("a generator that isn't a local".to_owned());
                    };
                    match self.function.locals.get(local.0).map(|local| &local.ty) {
                        Some(Type::Iterator(item)) => (**item).clone(),
                        _ => return Err("a generator whose items aren't known".to_owned()),
                    }
                };
                return Ok((self.module.kind(&item)?, header(GEN_ITEM)));
            }
            _ => {}
        }
        let def = def.ok_or("it reads a generator's locals from outside it")?;
        let at = number - slot::LOCALS;
        let ty = def.slots.get(at).ok_or("no such slot")?;
        let id = self.module.layouts.id(&def.name).ok_or("a generator with no layout")?;
        let layout = self.module.layouts.get(id).ok_or("a generator with no layout")?;
        let name = format!("\0slot{at}");
        let field = layout.fields.iter().find(|field| field.name == name).ok_or("no such slot")?;
        Ok((self.module.kind(ty)?, field.offset + header(FIELDS)))
    }

    pub(crate) fn make_generator(
        &mut self,
        dest: LocalId,
        generator: &str,
        args: &[(usize, Operand)],
    ) -> Result<(), String> {
        let def = self.module.generator(generator).ok_or("no such generator")?;
        // A closure outside the runtime's table can't be in a block of its.
        if !self.module.whole.runtime_closures && def.slots.iter().any(|ty| matches!(ty, Type::Arrow(..))) {
            return Err("it keeps a closure in a generator, which this backend doesn't take yet".to_owned());
        }
        let id = self.module.layouts.id(&def.name).ok_or("a generator with no layout")?;
        let resume = self.module.function_named(&def.resume)?;
        self.module.declare_call(resume)?;
        let block = self.new_block(id)?;
        let at = self.offset(&block, header(GEN_RESUME));
        self.line(&format!("store ptr {}, ptr {at}", symbol(&def.resume)));
        for (number, value) in args {
            let (kind, offset) = self.slot_place(Some(def), value, *number)?;
            let at = self.offset(&block, offset);
            self.fill(&kind, value, &at, "")?;
        }
        self.assign(dest, &block)
    }

    pub(crate) fn get_slot(
        &mut self,
        dest: LocalId,
        generator: &Operand,
        number: usize,
    ) -> Result<(), String> {
        let (kind, offset) = self.slot_place(self.own_generator(generator), generator, number)?;
        let (block, _) = self.operand(generator)?;
        let at = self.offset(&block, offset);
        if number == slot::FAILURE {
            return self.read_failure(dest, &at);
        }
        self.read_into(dest, &kind, &at, "")
    }

    /// Lets go of a generator made for a loop as the loop ends, which closes what it's holding if
    /// it stopped partway.
    pub(crate) fn let_go_of_generator(&mut self, generator: LocalId) {
        let (slot, held) = (self.slot(generator), self.temp());
        self.line(&format!("{held} = load ptr, ptr {slot}"));
        self.line(&format!("store ptr null, ptr {slot}"));
        self.release(&held);
    }

    /// What a generator failed with, from the block its slot holds the address of.
    fn read_failure(&mut self, dest: LocalId, at: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let block = self.temp();
        self.line(&format!("{block} = load ptr, ptr {at}"));
        if kind.in_memory() {
            let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
            return self.read_into(dest, &kind, &fields, "");
        }
        self.retain_value(&block, &kind);
        self.assign(dest, &block)
    }

    /// Puts what a generator failed with in a block its slot holds the address of. It stays there
    /// for whatever asks, however many times, so the block is only let go of with the program.
    fn write_failure(&mut self, value: &Operand, at: &str) -> Result<(), String> {
        let (value, kind) = self.operand(value)?;
        let block = match kind {
            Kind::Struct(id) | Kind::Enum(id) => {
                let block = self.new_block(id)?;
                let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                self.store_field(&kind, &value, &fields, "");
                self.retain_inline(&kind, &fields);
                block
            }
            _ if kind.counted() => {
                self.retain_value(&value, &kind);
                value
            }
            other => {
                return Err(format!(
                    "a generator fails with a {other:?}, which this backend doesn't take yet"
                ));
            }
        };
        self.line(&format!("store ptr {block}, ptr {at}"));
        Ok(())
    }

    /// The slot gets an owner of the value, and lets go of what it held.
    pub(crate) fn set_slot(
        &mut self,
        generator: &Operand,
        number: usize,
        value: &Operand,
    ) -> Result<(), String> {
        let (kind, offset) = self.slot_place(self.own_generator(generator), generator, number)?;
        if number == slot::FAILURE {
            let (block, _) = self.operand(generator)?;
            let at = self.offset(&block, offset);
            return self.write_failure(value, &at);
        }
        let (value, fresh) = self.value_for(value, &kind)?;
        let (block, _) = self.operand(generator)?;
        let at = self.offset(&block, offset);
        if kind.counted() {
            if !fresh {
                self.retain_value(&value, &kind);
            }
            let (old, text) = (self.temp(), self.text_of(&kind));
            self.line(&format!("{old} = load {text}, ptr {at}"));
            self.line(&format!("store {text} {value}, ptr {at}"));
            self.release_kind(&old, &kind);
        } else {
            self.store_field(&kind, &value, &at, "");
        }
        Ok(())
    }

    /// Runs a generator on to its next item: directly when the IR knows which it is, or through
    /// its block. Either way the flag comes back, as every generator's function takes it.
    pub(crate) fn resume(
        &mut self,
        dest: LocalId,
        generator: &Operand,
        known: Option<&str>,
        span: Span,
    ) -> Result<(), String> {
        let (block, _) = self.operand(generator)?;
        let target = if let Some(name) = known {
            let callee = self.module.function_named(name)?;
            self.module.declare_call(callee)?;
            symbol(name)
        } else {
            let at = self.offset(&block, header(GEN_RESUME));
            // A generator's resume function never changes while it lives.
            let code = self.temp();
            self.line(&format!("{code} = load ptr, ptr {at}, !invariant.load !1"));
            code
        };
        self.check_stack(span);
        let (more, held, failed) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{more} = call swiftcc i1 {target}(ptr {block}, ptr swifterror %err)"));
        self.line(&format!("{held} = load ptr, ptr %err"));
        self.line(&format!("{failed} = icmp ne ptr {held}, null"));
        self.leave_traced(&failed, span);
        self.assign(dest, &more)
    }

    /// Ends the program with a message the IR wrote: an `assert`, a `where` rule, or a generator
    /// used twice.
    pub(crate) fn stop(&mut self, message: &str, span: Span) {
        let text = self.literal(message);
        self.stop_with(&text, span);
    }

    /// Ends the program with a string's text.
    pub(crate) fn stop_with(&mut self, text: &str, span: Span) {
        self.stop_then(text, span, "");
    }

    /// Ends the program with a string's text, doing `then` once the failure's set.
    pub(crate) fn stop_then(&mut self, text: &str, span: Span, then: &str) {
        self.declare("declare void @fennec_stop(ptr, i64, i64) #1");
        let label = self.label("stop");
        let (place, args) = self.span_values(&label, span);
        for line in place.lines() {
            self.line(line.trim_start());
        }
        self.line(&format!("call void @fennec_stop(ptr {text}, {args})"));
        if !then.is_empty() {
            self.line(then);
        }
        // Stopping is what the program says to do, not a check.
        self.next_check(span, None);
        let failed = self.failed_exit();
        self.line(&failed);
    }

    /// `result.expect(reason)`: what it holds, or the end of the program, saying why.
    pub(crate) fn expect(
        &mut self,
        dest: Option<LocalId>,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let [result, Operand::Str(reason)] = args else {
            return Err("it calls `expect` without a reason written out".to_owned());
        };
        let (base, kind) = self.operand(result)?;
        let (number, fields) = self.case_of(&kind, "Err")?;
        let (_, offset, held) = fields.into_iter().find(|(name, ..)| name == "error").ok_or("no error")?;
        let held = held.ok_or("an error this backend doesn't take yet")?;
        if !matches!(
            held,
            Kind::Boxed(_) | Kind::Str | Kind::List(_) | Kind::Dict(..) | Kind::Struct(_) | Kind::Enum(_)
        ) {
            return Err(format!(
                "it expects a result whose error is a {held:?}, which this backend doesn't take yet"
            ));
        }
        let fields_at = self.fields_at(&base, &kind);
        let (tag, failed) = (self.temp(), self.temp());
        self.line(&format!("{tag} = load i64, ptr {fields_at}"));
        self.line(&format!("{failed} = icmp eq i64 {tag}, {number}"));
        let (bad, ok) = (self.label("unexpected"), self.label("expected"));
        self.line(&format!("br i1 {failed}, label %{bad}, label %{ok}, !prof !0"));
        self.start_block(&bad);
        self.declare("declare ptr @fennec_block_text(ptr) nounwind");
        self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
        let at = self.offset(&fields_at, offset);
        let (error, text, message) = (self.temp(), self.temp(), self.temp());
        // An error in place goes across as a block made of it.
        if let Kind::Struct(id) | Kind::Enum(id) = held {
            let block = self.new_block(id)?;
            let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
            self.store_field(&held, &at, &fields, "");
            self.retain_inline(&held, &fields);
            self.line(&format!("{error} = getelementptr inbounds i8, ptr {block}, i64 0"));
        } else if held == Kind::Str && self.headers() {
            let header = self.temp();
            self.line(&format!("{header} = load {}, ptr {at}", crate::headers::HEADER));
            let block = self.owned_text_block(&header);
            self.line(&format!("{error} = getelementptr inbounds i8, ptr {block}, i64 0"));
        } else {
            self.line(&format!("{error} = load ptr, ptr {at}"));
        }
        self.line(&format!("{text} = call ptr @fennec_block_text(ptr {error})"));
        let prefix = self.literal(&format!("this was expected to succeed ({reason}), but it failed: "));
        self.line(&format!("{message} = call ptr @fennec_str_concat(ptr {prefix}, ptr {text})"));
        // Its notes go under it, a line each.
        let message = self.with_notes_under(&message, &at, &held)?;
        self.stop_with(&message, span);
        self.start_block(&ok);
        match dest {
            Some(dest) => self.get_case_field(dest, result, "Ok", "value"),
            None => Ok(()),
        }
    }
}
