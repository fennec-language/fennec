//! The methods of the `Entropy` capability: secure random bytes from the operating system,
//! and seeds for `Random`. In a test, `Entropy` is a fake whose output is fixed, so every run
//! of the test draws the same bytes in the same order.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Capability, Value};

/// The seed of the fake's bytes, which are `SplitMix64` drawn in order.
const FAKE_SEED: i64 = 0;

impl Interp<'_> {
    pub(crate) fn entropy_method(
        &mut self,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match (method, args) {
            ("bytes", [Value::Int(count)]) => {
                let Ok(count) = usize::try_from(*count) else {
                    return Err(Diagnostic::new(format!("`bytes` can't give {count} bytes"), span)
                        .with_help("give it how many bytes you want, 0 or more"));
                };
                Ok(Value::Bytes(Rc::from(self.draw_entropy(count, span)?)))
            }
            ("seed", []) => {
                let drawn = self.draw_entropy(8, span)?;
                let word = drawn
                    .try_into()
                    .map_err(|_| Diagnostic::new("the operating system gave too few bytes", span))?;
                Ok(Value::Int(i64::from_le_bytes(word)))
            }
            ("bytes" | "seed", _) => Err(Diagnostic::new(format!("wrong arguments to `{method}`"), span)),
            _ => Err(no_method(&Value::Capability(Capability::Entropy), method, span)),
        }
    }

    /// `count` bytes: the test fake's next ones, or the operating system's.
    fn draw_entropy(&mut self, count: usize, span: Span) -> Result<Vec<u8>, Diagnostic> {
        if let Some(world) = self.world.as_mut().filter(|world| !world.real()) {
            let start = world.entropy_drawn;
            world.entropy_drawn += count;
            return Ok((start..start + count).map(fake_byte).collect());
        }
        let mut bytes = vec![0; count];
        getrandom::fill(&mut bytes).map_err(|err| {
            Diagnostic::new(format!("the operating system gave no random bytes: {err}"), span)
        })?;
        Ok(bytes)
    }
}

/// The fake's `at`th byte.
fn fake_byte(at: usize) -> u8 {
    let word = crate::stdlib::random::splitmix(FAKE_SEED, i64::try_from(at / 8).unwrap_or(i64::MAX));
    word.to_le_bytes().get(at % 8).copied().unwrap_or(0)
}
