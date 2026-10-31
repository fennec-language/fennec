//! Leaving a function, and what a fallible function gives back. A fallible function whose
//! value is a counted block returns the block's address, which is always even, or else a
//! failure's temporary's number shifted up with the low bit set. So `?` on a call to one just
//! tests a bit, and the value never has to become an interpreter value. Anything else that
//! uses the result gets an object, as from any other fallible function.

use cranelift_codegen::ir::{InstBuilder, Value, types};
use fennec_syntax::{Diagnostic, Span};

use super::expr::Lowered;
use super::{Lowerer, abi, internal};
use crate::helpers::Helper;
use crate::types::Ty;

impl Lowerer<'_, '_> {
    /// Leaves the function from here with a value.
    pub(super) fn ret(&mut self, value: Option<Value>, ty: Ty, span: Span) -> Result<(), Diagnostic> {
        self.put_back_all()?;
        // What's given back outlives every name, so it needs an owner of its own.
        let value = value.map(|value| self.own(value, ty));
        self.release_scopes(0)?;
        let function = self.function;
        let value = if let Some(direct) = function.direct {
            Some(self.convert(value, ty, direct, span)?)
        } else if function.fallible {
            let handle = self.handle_of(value, ty, span)?;
            Some(self.help(Helper::Ok, &[handle])?)
        } else {
            match function.ret {
                Ty::Unit | Ty::Cap(_) => None,
                Ty::Struct(id) => {
                    let value = self.convert(value, ty, Ty::Struct(id), span)?;
                    let dest = self.dest.ok_or_else(|| internal("no destination", span))?;
                    self.copy_struct(dest, value, id)?;
                    None
                }
                ret => Some(self.convert(value, ty, ret, span)?),
            }
        };
        self.jump_exit(value);
        Ok(())
    }

    /// Leaves the function with a value that's already what it returns, like a failure
    /// `?` passes up.
    pub(super) fn pass_up(&mut self, handle: Value) -> Result<(), Diagnostic> {
        self.put_back_all()?;
        self.release_scopes(0)?;
        let handle = if self.function.direct.is_some() { self.tagged(handle) } else { handle };
        let value = abi(self.function.ret).map(|_| handle);
        self.jump_exit(value);
        Ok(())
    }

    fn jump_exit(&mut self, value: Option<Value>) {
        let args: Vec<_> = value.into_iter().map(Into::into).collect();
        self.b.ins().jump(self.exit, &args);
        self.after_terminator();
    }

    /// A temporary's number as a function that returns blocks directly gives it back.
    fn tagged(&mut self, handle: Value) -> Value {
        let shifted = self.b.ins().ishl_imm_u(handle, 1);
        self.b.ins().bor_imm_u(shifted, 1)
    }

    /// Keeps what a function gives back as its frame ends: a failure's temporary, moved down,
    /// or a block, which needs no temporaries at all.
    pub(super) fn keep_direct(&mut self, base: Value, word: Value) -> Result<Value, Diagnostic> {
        let [failed, fine, done] = [(); 3].map(|()| self.b.create_block());
        self.b.append_block_param(done, types::I64);
        self.b.set_cold_block(failed);
        let tag = self.b.ins().band_imm_u(word, 1);
        self.b.ins().brif(tag, failed, &[], fine, &[]);
        self.b.switch_to_block(failed);
        self.b.seal_block(failed);
        let handle = self.b.ins().ushr_imm_u(word, 1);
        let kept = self.help(Helper::Keep, &[base, handle])?;
        let kept = self.tagged(kept);
        self.b.ins().jump(done, &[kept.into()]);
        self.b.switch_to_block(fine);
        self.b.seal_block(fine);
        self.call_helper(Helper::Truncate, &[base])?;
        self.b.ins().jump(done, &[word.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        let word = self.b.block_params(done).first().copied();
        word.ok_or_else(|| internal("lost a result", self.function.def.span))
    }

    /// What a call to a function gives back: as it is, when `?` is about to test this very
    /// call, or else an object.
    pub(super) fn called_result(
        &mut self,
        value: Option<Value>,
        ret: Ty,
        direct: Option<Ty>,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let (Some(direct), Some(word)) = (direct, value) else { return Ok((value, ret)) };
        if self.tried == Some(span) {
            self.tried = None;
            self.direct_result = Some(direct);
            return Ok((Some(word), ret));
        }
        let [failed, fine, done] = [(); 3].map(|()| self.b.create_block());
        self.b.append_block_param(done, types::I64);
        let tag = self.b.ins().band_imm_u(word, 1);
        self.b.ins().brif(tag, failed, &[], fine, &[]);
        self.b.switch_to_block(failed);
        self.b.seal_block(failed);
        let handle = self.b.ins().ushr_imm_u(word, 1);
        self.b.ins().jump(done, &[handle.into()]);
        self.b.switch_to_block(fine);
        self.b.seal_block(fine);
        let handle = self.handle_of(Some(word), direct, span)?;
        let handle = self.help(Helper::Ok, &[handle])?;
        self.b.ins().jump(done, &[handle.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        let handle = self.b.block_params(done).first().copied();
        Ok((handle, Ty::Obj))
    }

    /// Lowers the call `?` is on, giving back the word it returned and what it holds when
    /// it's a call to a function that returns a block directly.
    pub(super) fn tried_call(
        &mut self,
        value: &fennec_syntax::Expr,
    ) -> Result<(Lowered, Option<Ty>), Diagnostic> {
        self.tried = Some(value.span);
        let lowered = self.expr(value);
        self.tried = None;
        Ok((lowered?, self.direct_result.take()))
    }

    /// Whether a word from such a call is a failure, and the failure's temporary.
    pub(super) fn failure_of(&mut self, word: Value) -> (Value, Value) {
        let tag = self.b.ins().band_imm_u(word, 1);
        (tag, word)
    }

    /// The temporary a failure's word holds.
    pub(super) fn failure_handle(&mut self, word: Value) -> Value {
        self.b.ins().ushr_imm_u(word, 1)
    }
}
