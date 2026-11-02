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
        let method = name.rsplit('.').next().unwrap_or(name);
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.declare("declare i64 @fennec_mark() nounwind");
        self.declare("declare void @fennec_truncate(i64) nounwind");
        self.declare("declare void @fennec_input(i64) nounwind");
        let mark = self.temp();
        self.line(&format!("{mark} = call i64 @fennec_mark()"));
        let mut crossed = Vec::new();
        for arg in rest {
            let (value, kind) = self.operand(arg)?;
            let (handle, block) = self.value_handle(&value, &kind)?;
            self.line(&format!("call void @fennec_input(i64 {handle})"));
            crossed.extend(block);
        }
        let (word, _) = self.operand(receiver)?;
        let named = self.literal(method);
        self.declare("declare i64 @fennec_capability_call(ptr, ptr, i64, i64, i64) nounwind");
        let handle = self.temp();
        self.line(&format!(
            "{handle} = call i64 @fennec_capability_call(ptr {word}, ptr {named}, i64 {}, i64 {from}, i64 {to})",
            rest.len()
        ));
        self.leave_if_failed();
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
            let block = self.result_from(dest, &handle, span)?;
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
                self.native_at(&handle, &kind, &slot, span)?;
            }
            _ => {
                // A word, as inside a struct, then read back as the local holds it.
                let place = self.place_for(&Kind::Int);
                self.native_at(&handle, &kind, &place, span)?;
                let value = self.load_field(&kind, &place, "");
                self.assign(dest, &value)?;
            }
        }
        self.line(&format!("call void @fennec_truncate(i64 {mark})"));
        Ok(())
    }
}
