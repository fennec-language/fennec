//! The methods of the `Entropy` capability: secure random bytes from the operating system,
//! and seeds for `Random`. In a test, `Entropy` is a fake whose output is fixed, so every run
//! of the test draws the same bytes in the same order.


use fennec_syntax::{Diagnostic, Span};


/// The seed of the fake's bytes, which are `SplitMix64` drawn in order.
const FAKE_SEED: i64 = 0;

/// `count` secure random bytes from the operating system.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn os_random(count: usize, span: Span) -> Result<Vec<u8>, Diagnostic> {
    let mut bytes = vec![0; count];
    getrandom::fill(&mut bytes)
        .map_err(|err| Diagnostic::new(format!("the operating system gave no random bytes: {err}"), span))?;
    Ok(bytes)
}

/// The playground runs in a browser, with no operating system to ask.
#[cfg(target_arch = "wasm32")]
pub(crate) fn os_random(_: usize, span: Span) -> Result<Vec<u8>, Diagnostic> {
    Err(Diagnostic::new("`Entropy` isn't available in the playground", span)
        .with_help("a test's `entropy: Entropy` is a fake with fixed bytes, and works here"))
}

/// The fake's `at`th byte.
pub(crate) fn fake_byte(at: usize) -> u8 {
    let word = crate::stdlib::random::splitmix(FAKE_SEED, i64::try_from(at / 8).unwrap_or(i64::MAX));
    word.to_le_bytes().get(at % 8).copied().unwrap_or(0)
}
