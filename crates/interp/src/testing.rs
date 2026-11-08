//! What `fennec test` shares with the runtime's tests: how many inputs a property test tries, how
//! one test went, and where a test's random numbers start.

use fennec_syntax::Diagnostic;

/// How many inputs a property test tries.
pub const PROPERTY_CASES: usize = 100;

/// One test's result.
pub struct TestOutcome {
    pub name: String,
    pub failure: Option<Diagnostic>,
    /// How many inputs ran: 1 for a plain test.
    pub cases: usize,
}

/// What a test's name starts its random numbers from. It's below `RANDOM_MODULUS`, and never 0.
#[must_use]
pub fn property_seed(name: &str) -> i64 {
    let hash = name.bytes().fold(0x9e37_79b9_7f4a_7c15_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    i64::try_from(hash % (RANDOM_MODULUS - 1)).unwrap_or(0) + 1
}

/// The modulus of the random numbers, a prime. Each number times `RANDOM_FACTOR` fits in an `int`,
/// so `fennec test` can write the same numbers as Fennec, for tests that go through LLVM.
const RANDOM_MODULUS: u64 = 2_147_483_647;
