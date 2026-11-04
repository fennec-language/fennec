//! What a capability does, like `process.run(…)` or `fs.read_text(…)`: the runtime runs the
//! interpreter's own method on the capability's block, which says what a narrowed one may do,
//! with the arguments as temporaries, and what it gives back is made native here.

use fennec_ir::{LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::Emit;
use crate::kinds::Kind;

impl Emit<'_, '_, '_> {
    pub(crate) fn capability_call(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (receiver, rest) = args.split_first().ok_or("a method with nothing to call it on")?;
        // Any named arguments stay on the end, as a call writes them: `connect.timeout`.
        let method = name.split_once('.').map_or(name, |(_, method)| method);
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let (mark, crossed) = self.cross_arguments(rest)?;
        let (word, _) = self.operand(receiver)?;
        let named = self.literal(method);
        self.declare("declare i64 @fennec_capability_call(ptr, ptr, i64, i64, i64) nounwind");
        let handle = self.temp();
        self.line(&format!(
            "{handle} = call i64 @fennec_capability_call(ptr {word}, ptr {named}, i64 {}, i64 {from}, i64 {to})",
            rest.len()
        ));
        self.leave_if_failed();
        self.take_what_crossed(dest, &handle, (&mark, crossed), span)
    }

    /// `list(capability)`: what looping over it gives.
    pub(crate) fn capability_items(
        &mut self,
        dest: Option<LocalId>,
        capability: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let (mark, crossed) = self.cross_arguments(&[])?;
        let (word, _) = self.operand(capability)?;
        self.declare("declare i64 @fennec_capability_items(ptr, i64, i64) nounwind");
        let handle = self.temp();
        self.line(&format!("{handle} = call i64 @fennec_capability_items(ptr {word}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        self.take_what_crossed(dest, &handle, (&mark, crossed), span)
    }

    /// `capability[at]`.
    pub(crate) fn capability_item(
        &mut self,
        dest: LocalId,
        capability: &str,
        at: &str,
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let (mark, crossed) = self.cross_arguments(&[])?;
        self.declare("declare i64 @fennec_capability_item(ptr, i64, i64, i64) nounwind");
        let handle = self.temp();
        self.line(&format!(
            "{handle} = call i64 @fennec_capability_item(ptr {capability}, i64 {at}, i64 {from}, i64 {to})"
        ));
        self.leave_if_failed();
        self.take_what_crossed(Some(dest), &handle, (&mark, crossed), span)
    }

    /// A method of one of std's types that the runtime has, like `Connection.write`.
    pub(crate) fn std_method(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let (mark, crossed) = self.cross_arguments(args)?;
        let named = self.literal(name);
        self.declare("declare i64 @fennec_std_method(ptr, i64, i64, i64) nounwind");
        let handle = self.temp();
        self.line(&format!(
            "{handle} = call i64 @fennec_std_method(ptr {named}, i64 {}, i64 {from}, i64 {to})",
            args.len()
        ));
        self.leave_if_failed();
        self.take_what_crossed(dest, &handle, (&mark, crossed), span)
    }

    /// One of std's functions the runtime has, called as a capability's method is.
    pub(crate) fn std_call(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let (mark, crossed) = self.cross_arguments(args)?;
        let named = self.literal(name);
        // What `loads` reads into is the type the program wrote, here as a string.
        let wanted = match dest.and_then(|dest| self.function.locals.get(dest.0)).map(|local| &local.ty) {
            Some(fennec_check::Type::Result(wanted, _)) if name.contains(".loads") => {
                let written = wanted.to_string();
                self.literal(&written)
            }
            _ => "null".to_owned(),
        };
        self.declare("declare i64 @fennec_std_call(ptr, ptr, i64, i64, i64) nounwind");
        let handle = self.temp();
        self.line(&format!(
            "{handle} = call i64 @fennec_std_call(ptr {named}, ptr {wanted}, i64 {}, i64 {from}, i64 {to})",
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
