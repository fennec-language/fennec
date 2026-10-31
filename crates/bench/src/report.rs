//! What the measurements mean: each kernel's speed against its target and against the previous
//! commit, and which of them fail the suite.
//!
//! Startup and the work a kernel does are told apart. A kernel's speed is its work's: its
//! instructions minus what it takes to reach `main`, against the instructions its target takes.

use std::time::Duration;

use crate::targets::Target;

/// The JIT's floor: at least this fraction of the target's speed.
pub(crate) const JIT_FLOOR: f64 = 0.8;

/// How much slower than the previous commit a kernel may run before it fails. The median of
/// several runs on a shared CI machine still wobbles by several percent.
pub(crate) const NOISE: f64 = 0.10;

/// How many more instructions than the previous commit a kernel may run. Counted by
/// cachegrind, identical runs differ by about one part in ten million, even with the
/// runtime's hash seeds changing from run to run, so 1% leaves a wide margin.
pub(crate) const INSTRUCTION_NOISE: f64 = 0.01;

/// One kernel's measurements.
#[derive(Clone, Debug, Default)]
pub(crate) struct Result {
    pub(crate) kernel: String,
    /// How long the whole run takes, and how long it takes to reach `main`.
    pub(crate) total: Duration,
    pub(crate) startup: Duration,
    pub(crate) previous: Option<Duration>,
    /// Instructions for the whole run, for reaching `main`, and for the previous commit's
    /// whole run, where they were counted.
    pub(crate) instructions: Option<u64>,
    pub(crate) startup_instructions: Option<u64>,
    pub(crate) previous_instructions: Option<u64>,
    pub(crate) target: Option<Target>,
    /// How many places in its compiled code aren't native.
    pub(crate) not_native: Option<usize>,
    /// Whether the JIT handed any of it to the interpreter.
    pub(crate) fell_back: bool,
}

impl Result {
    /// The instructions the kernel's work takes, without reaching `main`.
    pub(crate) fn work_instructions(&self) -> Option<u64> {
        Some(self.instructions?.saturating_sub(self.startup_instructions?))
    }

    /// The speed of its work as a fraction of the target's: 1.0 is on target, 0.5 half as fast.
    pub(crate) fn speed(&self) -> Option<f64> {
        let target = self.target.as_ref()?;
        Some(as_f64(target.instructions) / as_f64(self.work_instructions()?).max(1.0))
    }

    /// The speed of its work against the target's by time, which is only as good as how alike
    /// this machine is to the one that pinned the target. Instructions and time can disagree a
    /// lot, in either direction, so the table shows both.
    pub(crate) fn speed_by_time(&self) -> Option<f64> {
        let target = self.target.as_ref().filter(|target| target.micros > 0)?;
        let work = self.total.saturating_sub(self.startup).as_secs_f64();
        (work > 0.0).then(|| as_f64(target.micros) / 1_000_000.0 / work)
    }

    pub(crate) fn meets_floor(&self) -> bool {
        self.speed().is_some_and(|speed| speed >= JIT_FLOOR)
    }

    /// Why this kernel breaks its ratchet, if it does.
    pub(crate) fn below_floor(&self) -> Option<String> {
        if !self.target.as_ref().is_some_and(|target| target.enforced) || self.meets_floor() {
            return None;
        }
        let speed = self.speed().map_or_else(|| "couldn't be counted".to_owned(), percent);
        Some(format!(
            "{} reached the floor of {} and can't drop below it: it's {speed}",
            self.kernel,
            percent(JIT_FLOOR)
        ))
    }

    /// How much slower than the previous commit, as a fraction: 0.2 is 20% slower.
    pub(crate) fn slowdown(&self) -> Option<f64> {
        self.previous
            .map(|previous| self.total.as_secs_f64() / previous.as_secs_f64().max(f64::MIN_POSITIVE) - 1.0)
    }

    /// How many more instructions than the previous commit, as a fraction, where both were
    /// counted.
    pub(crate) fn extra_instructions(&self) -> Option<f64> {
        let (now, before) = (self.instructions?, self.previous_instructions?);
        Some(as_f64(now) / as_f64(before).max(1.0) - 1.0)
    }

    /// Why this kernel counts as slower than before, if it does: it got slower than the noise
    /// allows, and, where instructions were counted, it also runs more of them. A slowdown the
    /// instruction count doesn't back up is the machine, not the compiler.
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
            millis(self.total)
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

fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

/// A count in millions of instructions.
fn millions(count: u64) -> String {
    format!("{:.1} M", as_f64(count) / 1_000_000.0)
}

fn or_dash(value: Option<String>) -> String {
    value.unwrap_or_else(|| "-".to_owned())
}

/// The results as a Markdown table.
pub(crate) fn table(results: &[Result]) -> String {
    let mut lines = vec![
        "| Kernel | Startup | Startup instructions | Work | Target time | % by time | Work instructions | Target instructions | % of target | Floor (80%) | Not native | Previous commit | Instructions vs previous |"
            .to_owned(),
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|".to_owned(),
    ];
    for result in results {
        let floor = match (result.speed(), result.target.as_ref().is_some_and(|target| target.enforced)) {
            (None, _) => "-",
            (Some(_), true) if result.meets_floor() => "met, enforced",
            (Some(_), true) => "**below**",
            (Some(_), false) if result.meets_floor() => "met",
            (Some(_), false) => "not yet",
        };
        let previous = match (result.previous, result.slowdown()) {
            (Some(previous), Some(slowdown)) => format!("{} ({:+.0}%)", millis(previous), slowdown * 100.0),
            _ => "-".to_owned(),
        };
        let extra = result.extra_instructions().map(|extra| format!("{:+.1}%", extra * 100.0));
        let fell_back = if result.fell_back { " (partly interpreted)" } else { "" };
        lines.push(format!(
            "| {} | {} | {} | {}{fell_back} | {} | {} | {} | {} | {} | {floor} | {} | {previous} | {} |",
            result.kernel,
            millis(result.startup),
            or_dash(result.startup_instructions.map(millions)),
            millis(result.total.saturating_sub(result.startup)),
            or_dash(result.target.as_ref().map(|target| millis(Duration::from_micros(target.micros)))),
            or_dash(result.speed_by_time().map(percent)),
            or_dash(result.work_instructions().map(millions)),
            or_dash(result.target.as_ref().map(|target| millions(target.instructions))),
            or_dash(result.speed().map(percent)),
            or_dash(result.not_native.map(|count| count.to_string())),
            or_dash(extra),
        ));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Result, table};
    use crate::targets::Target;

    fn timed(total: u64, previous: Option<u64>) -> Result {
        Result {
            kernel: "k".to_owned(),
            total: Duration::from_millis(total),
            previous: previous.map(Duration::from_millis),
            ..Result::default()
        }
    }

    /// A kernel counted at `work` instructions after 100 to start, with a target of `target`.
    fn counted(work: u64, target: u64, enforced: bool) -> Result {
        Result {
            kernel: "k".to_owned(),
            instructions: Some(100 + work),
            startup_instructions: Some(100),
            target: Some(Target {
                instructions: target,
                micros: 0,
                output: String::new(),
                source: String::new(),
                enforced,
            }),
            ..Result::default()
        }
    }

    #[test]
    fn speed_is_the_work_against_the_target_without_startup() {
        assert_eq!(counted(2000, 1000, false).speed(), Some(0.5));
        assert!(counted(1000, 810, false).meets_floor());
        assert!(!counted(1000, 790, false).meets_floor());
        assert_eq!(timed(100, None).speed(), None, "nothing was counted");
    }

    #[test]
    fn speed_by_time_is_the_work_against_the_targets_time() {
        let mut result = timed(30, None);
        result.startup = Duration::from_millis(10);
        let target = Target {
            instructions: 1,
            micros: 10_000,
            output: String::new(),
            source: String::new(),
            enforced: false,
        };
        result.target = Some(target);
        assert!((result.speed_by_time().unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(timed(30, None).speed_by_time(), None);
    }

    #[test]
    fn only_an_enforced_kernel_below_the_floor_fails() {
        assert_eq!(counted(1000, 790, false).below_floor(), None);
        assert_eq!(counted(1000, 800, true).below_floor(), None);
        let failure = counted(1000, 790, true).below_floor().unwrap();
        assert_eq!(failure, "k reached the floor of 80% and can't drop below it: it's 79%");
    }

    #[test]
    fn an_enforced_kernel_that_cant_be_counted_fails() {
        let mut uncounted = counted(1000, 900, true);
        uncounted.instructions = None;
        assert!(uncounted.below_floor().unwrap().contains("couldn't be counted"));
    }

    #[test]
    fn only_a_slowdown_past_the_noise_is_one() {
        assert_eq!(timed(110, Some(100)).failure(), None);
        assert_eq!(timed(90, Some(100)).failure(), None);
        assert_eq!(timed(100, None).failure(), None);
        let failure = timed(111, Some(100)).failure().unwrap();
        assert_eq!(failure, "k got 11% slower than the previous commit (100.0 ms before, 111.0 ms now)");
    }

    #[test]
    fn a_slowdown_counts_only_when_the_instructions_agree() {
        let with = |total, instructions, previous_instructions| Result {
            instructions: Some(instructions),
            previous_instructions: Some(previous_instructions),
            ..timed(total, Some(100))
        };
        assert_eq!(with(120, 1_005, 1_000).failure(), None, "the machine was busy");
        assert_eq!(with(105, 2_000, 1_000).failure(), None, "within the time noise");
        let failure = with(120, 1_100, 1_000).failure().unwrap();
        assert_eq!(
            failure,
            "k got 20% slower than the previous commit (100.0 ms before, 120.0 ms now), running 10.0% more instructions"
        );
    }

    #[test]
    fn the_table_tells_startup_and_work_apart_and_never_names_a_language() {
        let mut partly = Result {
            total: Duration::from_millis(30),
            startup: Duration::from_millis(10),
            previous: Some(Duration::from_millis(40)),
            fell_back: true,
            ..counted(2_000_000, 1_000_000, false)
        };
        partly.previous_instructions = partly.instructions;
        let met = counted(1_000_000, 900_000, true);
        let text = table(&[partly, met, timed(5, None)]);
        assert!(!text.contains("Rust"), "{text}");
        let rows: Vec<&str> = text.lines().skip(2).collect();
        assert_eq!(
            rows,
            [
                "| k | 10.0 ms | 0.0 M | 20.0 ms (partly interpreted) | 0.0 ms | - | 2.0 M | 1.0 M | 50% | not yet | - | 40.0 ms (-25%) | +0.0% |",
                "| k | 0.0 ms | 0.0 M | 0.0 ms | 0.0 ms | - | 1.0 M | 0.9 M | 90% | met, enforced | - | - | - |",
                "| k | 0.0 ms | - | 5.0 ms | - | - | - | - | - | - | - | - | - |",
            ]
        );
    }
}
