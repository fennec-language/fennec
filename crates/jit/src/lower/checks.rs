//! Leaving a function when a check fails: the jump to the failure, and carrying on when it
//! passes.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use fennec_syntax::Span;

use super::Lowerer;
use crate::helpers::Helper;

impl Lowerer<'_, '_> {
    /// Calls the runtime's failure with one of the JIT's own codes when `failed` is set,
    /// then leaves the function.
    pub(super) fn fail_if(&mut self, failed: Value, code: i64, span: Span) {
        let here = self.b.current_block();
        let fail = self.b.create_block();
        self.b.set_cold_block(fail);
        let ok = self.b.create_block();
        self.b.ins().brif(failed, fail, &[], ok, &[]);
        self.b.switch_to_block(fail);
        self.b.seal_block(fail);
        let code = self.int(code);
        let [start, end] = self.span_args(span);
        if let Some(id) = self.shared.helpers.get(&Helper::Fail).copied() {
            let callee = self.module.declare_func_in_func(id, self.b.func);
            self.b.ins().call(callee, &[code, start, end]);
        }
        self.b.ins().jump(self.bail, &[]);
        self.carry_on(here, ok);
        self.b.seal_block(ok);
    }

    /// Leaves the function if the program has failed.
    pub(super) fn check_failed(&mut self) {
        let address = self.failed_flag;
        let failed = self.b.ins().load(types::I8, MemFlagsData::trusted(), address, 0);
        self.leave_if(failed);
    }

    /// Leaves the function if `failed` is set.
    pub(super) fn leave_if(&mut self, failed: Value) {
        let (here, ok) = (self.b.current_block(), self.b.create_block());
        self.b.ins().brif(failed, self.bail, &[], ok, &[]);
        self.carry_on(here, ok);
        self.b.seal_block(ok);
    }
}
