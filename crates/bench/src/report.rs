//! What the timings mean: each kernel's speed against Rust and against the previous commit,
//! and which of them fail the suite.

use std::time::Duration;

/// The JIT's target: at least this fraction of Rust's speed.
pub(crate) const JIT_TARGET: f64 = 0.8;

/// How much slower than the previous commit a kernel may run before it fails. Timing on a
/// shared CI machine wobbles by a few percent even with the best of several runs.
pub(crate) const NOISE: f64 = 0.10;

/// One kernel's timings.
#[derive(Clone, Debug)]
pub(crate) struct Result {
    pub(crate) kernel: String,
    pub(crate) rust: Duration,
    pub(crate) fennec: Duration,
    pub(crate) previous: Option<Duration>,
    /// Whether the JIT handed any of it to the interpreter.
    pub(crate) fell_back: bool,
}

impl Result {
    /// Fennec's speed as a fraction of Rust's: 1.0 is as fast, 0.5 half as fast.
    pub(crate) fn speed(&self) -> f64 {
        self.rust.as_secs_f64() / self.fennec.as_secs_f64().max(f64::MIN_POSITIVE)
    }

    /// How much slower than the previous commit, as a fraction: 0.2 is 20% slower.
    pub(crate) fn slowdown(&self) -> Option<f64> {
        self.previous
            .map(|previous| self.fennec.as_secs_f64() / previous.as_secs_f64().max(f64::MIN_POSITIVE) - 1.0)
    }

    pub(crate) fn meets_target(&self) -> bool {
        self.speed() >= JIT_TARGET
    }

    /// Why this kernel fails the suite, if it does.
    pub(crate) fn failure(&self) -> Option<String> {
        let slowdown = self.slowdown()?;
        (slowdown > NOISE).then(|| {
            format!(
                "{} got {:.0}% slower than the previous commit ({} before, {} now)",
                self.kernel,
                slowdown * 100.0,
                millis(self.previous.unwrap_or_default()),
                millis(self.fennec)
            )
        })
    }
}

pub(crate) fn millis(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

/// The results as a Markdown table.
pub(crate) fn table(results: &[Result]) -> String {
    let mut lines = vec![
        "| Kernel | Rust | Fennec JIT | Speed vs Rust | JIT target (80%) | Previous commit |".to_owned(),
        "|---|---|---|---|---|---|".to_owned(),
    ];
    for result in results {
        let target = if result.meets_target() { "met" } else { "not yet" };
        let previous = match (result.previous, result.slowdown()) {
            (Some(previous), Some(slowdown)) => format!("{} ({:+.0}%)", millis(previous), slowdown * 100.0),
            _ => "-".to_owned(),
        };
        let fell_back = if result.fell_back { " (partly interpreted)" } else { "" };
        lines.push(format!(
            "| {} | {} | {}{fell_back} | {:.1}% | {target} | {previous} |",
            result.kernel,
            millis(result.rust),
            millis(result.fennec),
            result.speed() * 100.0
        ));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::{Result, table};
    use std::time::Duration;

    fn result(rust: u64, fennec: u64, previous: Option<u64>) -> Result {
        Result {
            kernel: "k".to_owned(),
            rust: Duration::from_millis(rust),
            fennec: Duration::from_millis(fennec),
            previous: previous.map(Duration::from_millis),
            fell_back: false,
        }
    }

    #[test]
    fn speed_is_against_rust() {
        assert!((result(10, 20, None).speed() - 0.5).abs() < 1e-9);
        assert!(result(81, 100, None).meets_target());
        assert!(!result(79, 100, None).meets_target());
    }

    #[test]
    fn only_a_slowdown_past_the_noise_fails() {
        assert_eq!(result(10, 110, Some(100)).failure(), None);
        assert_eq!(result(10, 90, Some(100)).failure(), None);
        assert_eq!(result(10, 100, None).failure(), None);
        let failure = result(10, 111, Some(100)).failure().unwrap();
        assert_eq!(failure, "k got 11% slower than the previous commit (100.0 ms before, 111.0 ms now)");
    }

    #[test]
    fn the_table_has_a_row_per_kernel() {
        let mut fell_back = result(10, 20, Some(25));
        fell_back.fell_back = true;
        let text = table(&[result(90, 100, None), fell_back]);
        assert_eq!(
            text,
            "| Kernel | Rust | Fennec JIT | Speed vs Rust | JIT target (80%) | Previous commit |\n\
             |---|---|---|---|---|---|\n\
             | k | 90.0 ms | 100.0 ms | 90.0% | met | - |\n\
             | k | 10.0 ms | 20.0 ms (partly interpreted) | 50.0% | not yet | 25.0 ms (-20%) |\n"
        );
    }
}
