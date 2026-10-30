//! What the timings mean: each kernel's speed against Rust and against the previous commit,
//! and which of them fail the suite.

use std::time::Duration;

/// The JIT's target: at least this fraction of Rust's speed.
pub(crate) const JIT_TARGET: f64 = 0.8;

/// How much slower than the previous commit a kernel may run before it fails. The median of
/// several runs on a shared CI machine still wobbles by several percent.
pub(crate) const NOISE: f64 = 0.10;

/// How many more instructions than the previous commit a kernel may run. Counts barely move
/// between runs of the same code, but the runtime's hash seeds differ from run to run.
pub(crate) const INSTRUCTION_NOISE: f64 = 0.03;

/// One kernel's timings.
#[derive(Clone, Debug)]
pub(crate) struct Result {
    pub(crate) kernel: String,
    pub(crate) rust: Duration,
    pub(crate) fennec: Duration,
    pub(crate) previous: Option<Duration>,
    /// Instructions counted for this commit's and the previous commit's runs, where `perf`
    /// could count them.
    pub(crate) instructions: Option<u64>,
    pub(crate) previous_instructions: Option<u64>,
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

    /// How many more instructions than the previous commit, as a fraction, where both were
    /// counted.
    pub(crate) fn extra_instructions(&self) -> Option<f64> {
        let (now, before) = (self.instructions?, self.previous_instructions?);
        Some(as_f64(now) / as_f64(before).max(1.0) - 1.0)
    }

    /// Why this kernel fails the suite, if it does: it got slower than the noise allows, and,
    /// where instructions were counted, it also runs more of them. A slowdown the instruction
    /// count doesn't back up is the machine, not the compiler.
    pub(crate) fn failure(&self) -> Option<String> {
        let slowdown = self.slowdown()?;
        if slowdown <= NOISE || self.extra_instructions().is_some_and(|extra| extra <= INSTRUCTION_NOISE) {
            return None;
        }
        let counted = self
            .extra_instructions()
            .map_or_else(String::new, |extra| format!(", running {:.1}% more instructions", extra * 100.0));
        Some(format!(
            "{} got {:.0}% slower than the previous commit ({} before, {} now){counted}",
            self.kernel,
            slowdown * 100.0,
            millis(self.previous.unwrap_or_default()),
            millis(self.fennec)
        ))
    }
}

#[expect(clippy::cast_precision_loss, reason = "instruction counts are far below where f64 loses precision")]
fn as_f64(count: u64) -> f64 {
    count as f64
}

pub(crate) fn millis(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

/// The results as a Markdown table.
pub(crate) fn table(results: &[Result]) -> String {
    let mut lines = vec![
        "| Kernel | Rust | Fennec JIT | Speed vs Rust | JIT target (80%) | Previous commit | Instructions vs previous |"
            .to_owned(),
        "|---|---|---|---|---|---|---|".to_owned(),
    ];
    for result in results {
        let target = if result.meets_target() { "met" } else { "not yet" };
        let previous = match (result.previous, result.slowdown()) {
            (Some(previous), Some(slowdown)) => format!("{} ({:+.0}%)", millis(previous), slowdown * 100.0),
            _ => "-".to_owned(),
        };
        let instructions = result
            .extra_instructions()
            .map_or_else(|| "-".to_owned(), |extra| format!("{:+.1}%", extra * 100.0));
        let fell_back = if result.fell_back { " (partly interpreted)" } else { "" };
        lines.push(format!(
            "| {} | {} | {}{fell_back} | {:.1}% | {target} | {previous} | {instructions} |",
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
            instructions: None,
            previous_instructions: None,
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
        fell_back.instructions = Some(1_010);
        fell_back.previous_instructions = Some(1_000);
        let text = table(&[result(90, 100, None), fell_back]);
        assert_eq!(
            text,
            "| Kernel | Rust | Fennec JIT | Speed vs Rust | JIT target (80%) | Previous commit | Instructions vs previous |\n\
             |---|---|---|---|---|---|---|\n\
             | k | 90.0 ms | 100.0 ms | 90.0% | met | - | - |\n\
             | k | 10.0 ms | 20.0 ms (partly interpreted) | 50.0% | not yet | 25.0 ms (-20%) | +1.0% |\n"
        );
    }

    fn counted(fennec: u64, previous: u64, instructions: u64, previous_instructions: u64) -> Result {
        let mut result = result(10, fennec, Some(previous));
        result.instructions = Some(instructions);
        result.previous_instructions = Some(previous_instructions);
        result
    }

    #[test]
    fn a_slowdown_fails_only_when_the_instructions_agree() {
        assert_eq!(counted(120, 100, 1_020, 1_000).failure(), None, "the machine was busy");
        assert_eq!(counted(105, 100, 2_000, 1_000).failure(), None, "within the time noise");
        let failure = counted(120, 100, 1_100, 1_000).failure().unwrap();
        assert_eq!(
            failure,
            "k got 20% slower than the previous commit (100.0 ms before, 120.0 ms now), running 10.0% more instructions"
        );
    }
}
