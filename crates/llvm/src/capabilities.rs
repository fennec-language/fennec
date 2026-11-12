//! What a capability does, like `process.run(…)` or `fs.read_text(…)`: the runtime runs the
//! method on the capability's block, which says what a narrowed one may do, with the arguments as
//! temporaries, and what it gives back is made native here. In a test, the runtime asks the
//! test's fakes first.

use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_syntax::Span;

use crate::emit::{Emit, FIELD_ACCESS};
use crate::kinds::Kind;
use crate::lists::FIELDS_OFFSET;

impl Emit<'_, '_, '_> {
    /// A method of `Clock` or `Args`, which the runtime does. Says whether `name` is one.
    fn native_capability(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
    ) -> Result<bool, String> {
        match (name, args) {
            ("Clock.now", []) => {
                self.declare("declare i64 @fennec_clock_now() nounwind");
                let nanos = self.temp();
                self.line(&format!("{nanos} = call i64 @fennec_clock_now()"));
                // A replay whose recording doesn't match fails here.
                self.leave_if_failed();
                self.instant_into(dest.ok_or("no result")?, &nanos)?;
            }
            ("Clock.time", []) => {
                self.declare("declare i64 @fennec_clock_time() nounwind");
                let nanos = self.temp();
                self.line(&format!("{nanos} = call i64 @fennec_clock_time()"));
                // A replay whose recording doesn't match fails here.
                self.leave_if_failed();
                self.instant_into(dest.ok_or("no result")?, &nanos)?;
            }
            ("Clock.sleep", [duration]) => {
                let (base, kind) = self.operand(duration)?;
                let (offset, held) = self.field(&kind, "nanos")?;
                let fields = self.fields_at(&base, &kind);
                let at = self.offset(&fields, offset);
                let access = if matches!(kind, Kind::Boxed(_)) { FIELD_ACCESS } else { "" };
                let nanos = self.load_field(&held, &at, access);
                self.declare("declare void @fennec_clock_sleep(i64) nounwind");
                self.line(&format!("call void @fennec_clock_sleep(i64 {nanos})"));
                // A sleep cut short by a stop ends the task here.
                self.leave_if_failed();
            }
            ("Args.len", []) => {
                self.declare("declare i64 @fennec_args_len() nounwind");
                let count = self.temp();
                self.line(&format!("{count} = call i64 @fennec_args_len()"));
                self.assign(dest.ok_or("no result")?, &count)?;
            }
            ("Args.program", []) => {
                self.declare("declare ptr @fennec_args_program() nounwind");
                let block = self.temp();
                self.line(&format!("{block} = call ptr @fennec_args_program()"));
                let text = self.text_value(&block);
                self.assign(dest.ok_or("no result")?, &text)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Makes `dest`, an `Instant` or a `Time`, holding `nanos`.
    fn instant_into(&mut self, dest: LocalId, nanos: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let (offset, held) = self.field(&kind, "nanos")?;
        match kind {
            Kind::Struct(_) => {
                let at = self.slot(dest);
                let at = self.offset(&at, offset);
                self.store_field(&held, nanos, &at, "");
                Ok(())
            }
            Kind::Boxed(id) => {
                let block = self.new_block(id)?;
                let fields = self.offset(&block, FIELDS_OFFSET);
                let at = self.offset(&fields, offset);
                self.store_field(&held, nanos, &at, FIELD_ACCESS);
                self.assign(dest, &block)
            }
            other => Err(format!("the clock gives a {other:?}")),
        }
    }

    /// `args[index]`, as a new string, which leaves the function if it's out of range.
    pub(crate) fn args_item(&mut self, dest: LocalId, index: &Operand, span: Span) -> Result<(), String> {
        let (index, _) = self.operand(index)?;
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.declare("declare ptr @fennec_args_item(i64, i64, i64) nounwind");
        let block = self.temp();
        self.line(&format!("{block} = call ptr @fennec_args_item(i64 {index}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        let text = self.text_value(&block);
        self.assign(dest, &text)
    }

    /// A new list of the arguments, for `for arg in args`.
    pub(crate) fn args_list(&mut self, dest: LocalId) -> Result<(), String> {
        let strings = self.element_number(ListItem::Str);
        self.declare("declare ptr @fennec_args_list(i64) nounwind");
        let list = self.temp();
        self.line(&format!("{list} = call ptr @fennec_args_list(i64 {strings})"));
        self.assign(dest, &list)
    }

    pub(crate) fn capability_call(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (receiver, rest) = args.split_first().ok_or("a method with nothing to call it on")?;
        // A test's capabilities are fakes, which the runtime holds, and asks first.
        if self.native_capability(dest, name, rest)? {
            return Ok(());
        }
        // A named `timeout=` stays on the method's name: the runtime takes it off.
        let method = name.split_once('.').map_or(name, |(_, method)| method);
        if crate::parallel::is_request(name) {
            // This call is all that links TLS into a program.
            self.declare("declare void @fennec_use_tls() nounwind");
            self.line("call void @fennec_use_tls()");
        }
        let (word, _) = self.operand(receiver)?;
        self.crossing(dest, ("fennec_native_capability", "ptr", &word), method, rest, span)
    }

    /// A method of a `Connection`, `Server` or `UdpSocket`, which the runtime does by the handle it holds.
    pub(crate) fn socket_call(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (receiver, rest) = args.split_first().ok_or("a method with nothing to call it on")?;
        let handle = self.socket_handle(receiver)?;
        self.socket_method(dest, &handle, name, rest, span)
    }

    /// The handle a `Connection` or `Server` holds.
    pub(crate) fn socket_handle(&mut self, socket: &Operand) -> Result<String, String> {
        let (held, kind) = self.operand(socket)?;
        let inside = if matches!(kind, Kind::Boxed(_)) { self.offset(&held, FIELDS_OFFSET) } else { held };
        let (offset, _) = self.field(&kind, "_handle")?;
        let at = self.offset(&inside, offset);
        let handle = self.temp();
        self.line(&format!("{handle} = load i64, ptr {at}"));
        Ok(handle)
    }

    /// A method by a handle, which can park its task.
    pub(crate) fn socket_method(
        &mut self,
        dest: Option<LocalId>,
        handle: &str,
        name: &str,
        rest: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        if crate::parallel::is_request(name) {
            // This call is all that links TLS into a program.
            self.declare("declare void @fennec_use_tls() nounwind");
            self.line("call void @fennec_use_tls()");
        }
        self.crossing(dest, ("fennec_net_call", "i64", handle), name, rest, span)
    }

    /// `response.text()`: its body as text, with what isn't text replaced.
    pub(crate) fn response_text(&mut self, dest: LocalId, response: &Operand) -> Result<(), String> {
        let (held, kind) = self.operand(response)?;
        let boxed = matches!(kind, Kind::Boxed(_));
        let inside = if boxed { self.offset(&held, FIELDS_OFFSET) } else { held };
        let (offset, field) = self.field(&kind, "body")?;
        let at = self.offset(&inside, offset);
        let body = self.load_field(&field, &at, if boxed { FIELD_ACCESS } else { "" });
        self.declare("declare ptr @fennec_bytes_text(ptr) nounwind");
        let block = self.temp();
        self.line(&format!("{block} = call ptr @fennec_bytes_text(ptr {body})"));
        let text = self.text_value(&block);
        self.assign(dest, &text)
    }

    /// Hands arguments over to a method in the runtime, and makes what it gives back native.
    fn crossing(
        &mut self,
        dest: Option<LocalId>,
        (runtime, word_type, word): (&str, &str, &str),
        method: &str,
        rest: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let (mark, crossed) = self.cross_arguments(rest)?;
        let named = self.literal(method);
        self.declare(&format!("declare i64 @{runtime}({word_type}, ptr, i64, i64, i64) nounwind"));
        let handle = self.temp();
        self.line(&format!(
            "{handle} = call i64 @{runtime}({word_type} {word}, ptr {named}, i64 {}, i64 {from}, i64 {to})",
            rest.len()
        ));
        self.leave_if_failed();
        self.take_what_crossed(dest, &handle, (&mark, crossed), span)
    }

    /// One of `std.crypto`'s functions, which the runtime runs.
    pub(crate) fn crypto_call(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let (mark, crossed) = self.cross_arguments(args)?;
        let named = self.literal(name);
        self.declare("declare i64 @fennec_crypto_call(ptr, i64, i64, i64) nounwind");
        let handle = self.temp();
        self.line(&format!(
            "{handle} = call i64 @fennec_crypto_call(ptr {named}, i64 {}, i64 {from}, i64 {to})",
            args.len()
        ));
        self.leave_if_failed();
        self.take_what_crossed(dest, &handle, (&mark, crossed), span)
    }

    /// Hands the runtime arguments as temporaries: where they start, and the blocks that carry
    /// them across, to let go of after.
    fn cross_arguments(&mut self, args: &[Operand]) -> Result<(String, Vec<String>), String> {
        self.declare("declare i64 @fennec_mark() nounwind");
        self.declare("declare void @fennec_truncate(i64) nounwind");
        self.declare("declare void @fennec_input(i64) nounwind");
        let mark = self.temp();
        self.line(&format!("{mark} = call i64 @fennec_mark()"));
        let mut crossed = Vec::new();
        for arg in args {
            let (value, kind) = self.operand(arg)?;
            let (handle, block) = self.value_handle(&value, &kind)?;
            self.line(&format!("call void @fennec_input(i64 {handle})"));
            crossed.extend(block);
        }
        Ok((mark, crossed))
    }

    /// Makes native what the runtime gave back, and lets go of what crossed.
    fn take_what_crossed(
        &mut self,
        dest: Option<LocalId>,
        handle: &str,
        (mark, crossed): (&str, Vec<String>),
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        for block in crossed {
            self.release(&block);
        }
        let Some(dest) = dest else {
            self.line(&format!("call void @fennec_truncate(i64 {mark})"));
            return Ok(());
        };
        let kind = self.kind_of(dest)?.clone();
        let local = self.function.locals.get(dest.0).ok_or("no such local")?;
        if matches!(local.ty, fennec_check::Type::Result(..)) {
            let block = self.result_from(dest, handle, span)?;
            self.line(&format!("call void @fennec_truncate(i64 {mark})"));
            if let Some(block) = block {
                self.assign(dest, &block)?;
            }
            return Ok(());
        }
        match kind {
            Kind::Unit => {}
            // A capability it gives back, narrowed or not, as a block.
            Kind::Cap => {
                self.declare("declare ptr @fennec_capability_from_value(i64, i64, i64) nounwind");
                let block = self.temp();
                self.line(&format!(
                    "{block} = call ptr @fennec_capability_from_value(i64 {handle}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                self.assign(dest, &block)?;
            }
            _ if kind.in_memory() => {
                let slot = self.slot(dest);
                self.native_at(handle, &kind, &slot, span)?;
            }
            _ => {
                // A word, as inside a struct, then read back as the local holds it.
                let place = self.place_for(if kind == Kind::Bool { &Kind::Int } else { &kind });
                self.native_at(handle, &kind, &place, span)?;
                let value = self.load_field(&kind, &place, "");
                self.assign(dest, &value)?;
            }
        }
        self.line(&format!("call void @fennec_truncate(i64 {mark})"));
        Ok(())
    }
}
