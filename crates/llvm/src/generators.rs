//! Generators, as the IR builds them: a block holding where it stopped, its item and the locals
//! it keeps, and a function that runs it on to its next item, whose address is in the block.
//! Every generator's block starts the same way, so a loop reads one without knowing which it is.

use fennec_check::Type;
use fennec_interp::embed::layout::{FIELDS, GEN_ITEM, GEN_PC, GEN_RESUME, GEN_STARTED};
use fennec_ir::{GeneratorDef, LocalId, Operand, slot};
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
            slot::FAILURE => {
                return Err(
                    "it uses a generator that can fail, which this backend doesn't take yet".to_owned()
                );
            }
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
        if def.fails {
            return Err("it makes a generator that can fail, which this backend doesn't take yet".to_owned());
        }
        if def.slots.iter().any(|ty| matches!(ty, Type::Arrow(..))) {
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
        self.read_into(dest, &kind, &at, "")
    }

    /// The slot gets an owner of the value, and lets go of what it held.
    pub(crate) fn set_slot(
        &mut self,
        generator: &Operand,
        number: usize,
        value: &Operand,
    ) -> Result<(), String> {
        let (kind, offset) = self.slot_place(self.own_generator(generator), generator, number)?;
        let (value, fresh) = self.value_for(value, &kind)?;
        let (block, _) = self.operand(generator)?;
        let at = self.offset(&block, offset);
        if kind.counted() {
            if !fresh {
                self.retain_value(&value, &kind);
            }
            let old = self.temp();
            self.line(&format!("{old} = load ptr, ptr {at}"));
            self.line(&format!("store ptr {value}, ptr {at}"));
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
        self.leave_if(&failed);
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
        self.declare("declare void @fennec_stop(ptr, i64, i64) #1");
        let label = self.label("stop");
        let (place, args) = self.span_values(&label, span);
        for line in place.lines() {
            self.line(line.trim_start());
        }
        self.line(&format!("call void @fennec_stop(ptr {text}, {args})"));
        let failed = self.failed_exit();
        self.line(&failed);
    }

    /// `result.expect(reason)`: what it holds, or the end of the program, saying why in the
    /// interpreter's words.
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
        if !matches!(held, Kind::Boxed(_) | Kind::Str | Kind::List(_) | Kind::Dict(..)) {
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
        self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
        self.declare("declare ptr @fennec_value_text(i64) nounwind");
        self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
        let at = self.offset(&fields_at, offset);
        let (error, handle, text, message) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{error} = load ptr, ptr {at}"));
        self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {error})"));
        self.line(&format!("{text} = call ptr @fennec_value_text(i64 {handle})"));
        let prefix = self.literal(&format!("this was expected to succeed ({reason}), but it failed: "));
        self.line(&format!("{message} = call ptr @fennec_str_concat(ptr {prefix}, ptr {text})"));
        self.stop_with(&message, span);
        self.start_block(&ok);
        match dest {
            Some(dest) => self.get_case_field(dest, result, "Ok", "value"),
            None => Ok(()),
        }
    }
}
