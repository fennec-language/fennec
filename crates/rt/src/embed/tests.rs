//! Running several tests on one thread, one after another.

use fennec_syntax::Diagnostic;

use super::{STATE, set_failed};

/// Ends one test of several run on this thread, giving back how it failed, if it did, and
/// clearing what a failure left behind so the next one starts clean.
pub fn next_test() -> Option<Diagnostic> {
    set_failed(0);
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut()?;
        let failure = state.failure.take();
        if failure.is_some() {
            state.temps.clear();
            state.inputs.clear();
        }
        failure
    })
}
