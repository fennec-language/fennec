//! Running several tests on one thread, one after another.

use fennec_syntax::Diagnostic;

use super::{FAILED, STATE};
use crate::scope::Frame;

/// Whether a function of the program ran in the interpreter since this was last asked.
pub fn ran_in_interpreter() -> bool {
    STATE.with_borrow_mut(|state| {
        state.as_mut().is_some_and(|state| std::mem::take(&mut state.interp.ran_program_code))
    })
}

/// Ends one test of several run on this thread, giving back how it failed, if it did, and
/// clearing what a failure left behind so the next one starts clean.
pub fn next_test() -> Option<Diagnostic> {
    FAILED.set(0);
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut()?;
        let failure = state.failure.take();
        if failure.is_some() {
            state.temps.clear();
            state.inputs.clear();
            state.bindings.clear();
            state.path = None;
            state.leaving = None;
            state.callers.clear();
            state.interp.frame = Frame::new();
        }
        failure
    })
}
