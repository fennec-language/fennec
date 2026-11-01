//! `fennec-bench budgets`: how long the things done on every keystroke and save take on a big
//! kit, against the limits they must stay under. Writing, checking, testing and formatting have
//! to feel instant. Only warm numbers count, with the daemon running and the cache full; cold
//! ones are shown too.
//!
//! An edit's verdict is held to a count of instructions rather than to its time, because the
//! machines CI runs on differ by nearly a factor of two, and a pull request shouldn't fail for
//! landing on a slow one. The limit is what a second means on those machines, so the promise
//! stays one second. Its time is shown too, and warns.
//!
//! A budget that's met fails the run when it's exceeded. One that hasn't been met yet is a
//! warning, since it would fail every pull request; it starts failing runs once `enforced`
//! is set, which happens in the change that meets it.

use std::fmt::Write as _;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use fennec_count::{Counter, Program};
use fennec_lsp::Json;

use crate::report::millis;

/// A limit on how long something may take, warm.
#[derive(Clone, Copy)]
struct Budget {
    name: &'static str,
    limit: Duration,
    enforced: bool,
}

const FORMAT: Budget = Budget { name: "format one file", limit: Duration::from_millis(50), enforced: true };
const CHECK: Budget = Budget { name: "edit to checked", limit: Duration::from_millis(100), enforced: false };
const VERDICT: Budget =
    Budget { name: "edit to verdict, one function", limit: Duration::from_secs(1), enforced: true };
/// What an edit's verdict may run: about what a second is on CI's machines. It ran 6.66 billion
/// when this was written, counted by Valgrind, which took between 650 and 1170 ms depending on the
/// machine, about 7.4 billion a second in the middle.
const MOST_VERDICT_INSTRUCTIONS: u64 = 7_500_000_000;

const RUN: Budget =
    Budget { name: "`fennec run` startup", limit: Duration::from_millis(100), enforced: false };

/// How many times longer an edit's verdict may take on the big kit than on one a tenth its
/// size. It shouldn't depend on the size of a project at all; some allowance is for noise.
const MOST_GROWTH: f64 = 1.5;

/// How big a kit to measure on: files, each with this many functions and a tenth as many tests.
const FUNCTIONS_PER_FILE: usize = 100;

/// A budget and what was measured against it.
struct Measured {
    budget: Budget,
    warm: Duration,
    cold: Option<Duration>,
    /// What it's held to instead of its time, and what it counted, where that can be counted.
    counted: Option<Counted>,
}

/// A count of instructions and the most there may be.
#[derive(Clone, Copy)]
struct Counted {
    instructions: u64,
    most: u64,
}

impl Measured {
    fn over(&self) -> bool {
        self.warm > self.budget.limit
    }

    /// Whether this fails the run: an enforced budget that's over, by its count if it has one.
    fn fails(&self) -> bool {
        match self.counted {
            Some(Counted { instructions, most }) => instructions > most,
            None => self.budget.enforced && self.over(),
        }
    }

    fn verdict(&self) -> &'static str {
        match (self.fails(), self.over(), self.budget.enforced) {
            (true, ..) => "**over**",
            (false, false, _) => "met",
            (false, true, true) => "met by its count, over by the clock",
            (false, true, false) => "over, not enforced yet",
        }
    }
}

pub(crate) fn run(fennec: &Path, runs: usize, files: usize) -> Result<bool, String> {
    let fennec = std::fs::canonicalize(fennec).map_err(|err| format!("{}: {err}", fennec.display()))?;
    let dir = std::env::temp_dir().join(format!("fennec-budgets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let kit =
        Kit::write(&dir, fennec.clone(), files).map_err(|err| format!("couldn't write the kit: {err}"))?;
    let measured = measure(&kit, runs);
    let _ = kit.command(&["daemon", "stop"]).output();
    let _ = std::fs::remove_dir_all(&dir);
    let measured = measured?;
    let small = smaller_verdict(&dir, fennec, runs, files)?;
    let growth = measured
        .iter()
        .find(|measured| measured.budget.name == VERDICT.name)
        .map_or(0.0, |verdict| verdict.warm.as_secs_f64() / small.as_secs_f64().max(f64::MIN_POSITIVE));
    let lines = format!("{} files, {} functions", files + 1, files * FUNCTIONS_PER_FILE);
    let mut table = table(&measured, &lines);
    let _ = writeln!(table, "\n{}", growth_line(growth, small));
    print!("{table}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        crate::append(Path::new(&summary), &format!("## Instant-feedback budgets\n\n{table}"))
            .map_err(|err| format!("couldn't write the summary: {err}"))?;
    }
    for over in measured.iter().filter(|measured| measured.over() && !measured.fails()) {
        let Budget { name, limit, .. } = over.budget;
        println!("::warning title=Over budget::{name} took {}, over {}", millis(over.warm), millis(limit));
    }
    if growth > MOST_GROWTH {
        println!("::warning title=Over budget::{}", growth_line(growth, small));
    }
    Ok(passes(&measured))
}

/// How long an edit's verdict takes on a kit a tenth the size of the big one.
fn smaller_verdict(dir: &Path, fennec: PathBuf, runs: usize, files: usize) -> Result<Duration, String> {
    let kit = Kit::write(dir, fennec, (files / 10).max(EDITED_FILE + 1))
        .map_err(|err| format!("couldn't write the kit: {err}"))?;
    kit.timed(&["fmt"])?;
    let verdict = kit.verdict(runs);
    let _ = kit.command(&["daemon", "stop"]).output();
    let _ = std::fs::remove_dir_all(dir);
    Ok(verdict?.warm)
}

/// What growing a project tenfold does to an edit's verdict. It warns until #152 makes a
/// verdict independent of the size of the project.
fn growth_line(growth: f64, small: Duration) -> String {
    format!(
        "An edit's verdict takes {growth:.1} times as long on the big kit as on one a tenth its size ({}); it should take at most {MOST_GROWTH} times.",
        millis(small)
    )
}

/// Whether every enforced budget is met.
fn passes(measured: &[Measured]) -> bool {
    !measured.iter().any(Measured::fails)
}

fn table(measured: &[Measured], size: &str) -> String {
    let mut table = format!("On a kit of {size}.\n\n| | limit | warm | cold | |\n|---|---:|---:|---:|---|\n");
    for measured in measured {
        let cold = measured.cold.map_or_else(String::new, millis);
        let Budget { name, limit, .. } = measured.budget;
        let (limit, warm, verdict) = (millis(limit), millis(measured.warm), measured.verdict());
        let counted = measured.counted.map_or_else(String::new, |Counted { instructions, most }| {
            format!("{} of at most {} M instructions: ", instructions / 1_000_000, most / 1_000_000)
        });
        let _ = writeln!(table, "| {name} | {limit} | {warm} | {cold} | {counted}{verdict} |");
    }
    table
}

fn measure(kit: &Kit, runs: usize) -> Result<Vec<Measured>, String> {
    kit.timed(&["fmt"])?;
    let (check_warm, check_cold) = edit_to_checked(kit, runs)?;
    let mut measured = vec![
        Measured { budget: CHECK, warm: check_warm, cold: Some(check_cold), counted: None },
        kit.format(runs)?,
    ];
    measured.push(kit.verdict(runs)?);
    measured.push(kit.repeated(RUN, &["run"], runs)?);
    Ok(measured)
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

/// A big kit on disk, with a Fennec folder of its own.
struct Kit {
    dir: PathBuf,
    fennec: PathBuf,
}

/// The file edits are made to, and the line in it that changes.
const EDITED: &str = "src/m3.fen";
const EDITED_FILE: usize = 3;
const EDITED_LINE: &str = "    y = x + 300\n";

/// A function that checks and runs natively, and that nothing else depends on.
fn function(number: usize) -> String {
    format!(
        "def f{number}(x: int) -> int:\n    y = x + {number}\n    if y > 100:\n        return y - 1\n    \
         z = y * 2\n    z + 1\n\n"
    )
}

impl Kit {
    fn write(dir: &Path, fennec: PathBuf, files: usize) -> std::io::Result<Kit> {
        std::fs::create_dir_all(dir.join("src"))?;
        let manifest =
            format!("name = \"big\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
        std::fs::write(dir.join("kit.toml"), manifest)?;
        let mut imports: Vec<String> = Vec::new();
        for file in 0..files {
            let first = file * FUNCTIONS_PER_FILE;
            let mut text = String::new();
            for number in first..first + FUNCTIONS_PER_FILE {
                text.push_str(&function(number));
            }
            for number in (first..first + FUNCTIONS_PER_FILE).step_by(10) {
                let _ = write!(text, "test \"t{number}\":\n    assert f{number}(1) > 0\n\n");
            }
            std::fs::write(dir.join(format!("src/m{file}.fen")), text)?;
            imports.push(format!("from m{file} import f{first}\n"));
        }
        imports.sort();
        let calls: Vec<String> =
            (0..files).map(|file| format!("f{}(1)", file * FUNCTIONS_PER_FILE)).collect();
        let main = format!(
            "{}\ndef main(stdout: Stdout):\n    stdout.print({})\n",
            imports.concat(),
            calls.join(" + ")
        );
        std::fs::write(dir.join("src/main.fen"), main)?;
        Ok(Kit { dir: dir.to_path_buf(), fennec })
    }

    /// `fennec` in this kit, with the environment of a person's machine rather than CI's.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.fennec);
        command.args(args);
        self.set_up(&mut command);
        command
    }

    fn set_up(&self, command: &mut Command) {
        command
            .current_dir(&self.dir)
            .env("FENNEC_HOME", self.dir.join("home"))
            .env_remove("CI")
            .env_remove("FENNEC_NO_CACHE")
            .env_remove("FENNEC_NO_DAEMON");
    }

    /// How long a command takes, and what it prints. It has to succeed.
    fn timed(&self, args: &[&str]) -> Result<(Duration, String), String> {
        let start = Instant::now();
        let output = self.command(args).output().map_err(|err| format!("couldn't run fennec: {err}"))?;
        let took = start.elapsed();
        if !output.status.success() {
            let why = String::from_utf8_lossy(&output.stderr);
            return Err(format!("`fennec {}` failed:\n{why}", args.join(" ")));
        }
        Ok((took, String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    /// The median of running a command `runs` times, after a first run that fills the cache.
    fn repeated(&self, budget: Budget, args: &[&str], runs: usize) -> Result<Measured, String> {
        let (cold, _) = self.timed(args)?;
        let mut times = Vec::with_capacity(runs);
        for _ in 0..runs {
            times.push(self.timed(args)?.0);
        }
        Ok(Measured { budget, warm: median(times), cold: Some(cold), counted: None })
    }

    fn format(&self, runs: usize) -> Result<Measured, String> {
        let mut measured = self.repeated(FORMAT, &["fmt", "--check", EDITED], runs)?;
        measured.cold = None;
        Ok(measured)
    }

    /// From changing one function to `fennec test` having a pass or fail, each time, which must
    /// run exactly one test.
    fn verdict(&self, runs: usize) -> Result<Measured, String> {
        let path = self.dir.join(EDITED);
        let original = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
        let (cold, _) = self.timed(&["test"])?;
        let mut times = Vec::with_capacity(runs);
        for run in 1..=runs {
            let edited = original.replace(EDITED_LINE, &format!("    y = x + 300 + {run}\n"));
            std::fs::write(&path, edited).map_err(|err| err.to_string())?;
            let (took, printed) = self.timed(&["test"])?;
            if !ran_one_test(&printed) {
                return Err(format!("an edit to one function should run one test, but:\n{printed}"));
            }
            times.push(took);
        }
        let edited = original.replace(EDITED_LINE, "    y = x + 300 + 1000\n");
        std::fs::write(&path, edited).map_err(|err| err.to_string())?;
        let counted = self.counted_verdict();
        std::fs::write(&path, original).map_err(|err| err.to_string())?;
        Ok(Measured { budget: VERDICT, warm: median(times), cold: Some(cold), counted })
    }
}

/// Whether `fennec test` says it ran exactly one test and left the rest unchanged.
fn ran_one_test(printed: &str) -> bool {
    printed.contains("(1 run,")
}

impl Kit {
    /// How many instructions `fennec test` runs just after an edit, where they can be counted.
    fn counted_verdict(&self) -> Option<Counted> {
        let folder = self.dir.join("counting");
        std::fs::create_dir_all(&folder).ok()?;
        let counter = Counter::find(&folder)?;
        let program = Program { command: self.fennec.clone(), args: vec!["test".to_owned()], stdin: None };
        let instructions = counter.count_set_up(&program, |command| self.set_up(command))?;
        Some(Counted { instructions, most: MOST_VERDICT_INSTRUCTIONS })
    }
}

/// A language server session, which is how an edit gets checked.
struct Editor {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Editor {
    fn send(&mut self, message: &Json) -> Result<(), String> {
        fennec_lsp::write(&mut self.input, message).map_err(|err| err.to_string())
    }

    /// Sends a notification, then waits until the server has finished with it: it answers
    /// messages in order, so that's when a request sent after it comes back.
    fn settled(&mut self, method: &str, params: Json) -> Result<Duration, String> {
        let start = Instant::now();
        self.send(&Json::object([("method", Json::string(method)), ("params", params)]))?;
        self.send(&Json::object([("id", Json::number(1)), ("method", Json::string("fennec/settled"))]))?;
        loop {
            let message = fennec_lsp::read(&mut self.output).map_err(|err| err.to_string())?;
            let message = message.ok_or("the language server stopped")?;
            if message.get("id").is_some() {
                return Ok(start.elapsed());
            }
        }
    }
}

/// From sending a document's new text to having all of the program's diagnostics, warm and
/// cold.
fn edit_to_checked(kit: &Kit, runs: usize) -> Result<(Duration, Duration), String> {
    let mut child = kit
        .command(&["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("couldn't start the language server: {err}"))?;
    let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("the language server has no pipes".to_owned());
    };
    let mut editor = Editor { child, input, output: BufReader::new(output) };
    let path = std::fs::canonicalize(kit.dir.join(EDITED)).map_err(|err| err.to_string())?;
    let uri = fennec_lsp::uri_of(&path);
    let text = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let document = Json::object([
        ("uri", Json::string(uri.as_str())),
        ("languageId", Json::string("fennec")),
        ("version", Json::number(1)),
        ("text", Json::string(text.as_str())),
    ]);
    let cold = editor.settled("textDocument/didOpen", Json::object([("textDocument", document)]))?;
    let mut times = Vec::with_capacity(runs);
    for run in 1..=runs {
        let edited = text.replace(EDITED_LINE, &format!("    y = x + 300 + {run}\n"));
        let document =
            Json::object([("uri", Json::string(uri.as_str())), ("version", Json::number(run + 1))]);
        let changes = Json::Array(vec![Json::object([("text", Json::string(edited))])]);
        let params = Json::object([("textDocument", document), ("contentChanges", changes)]);
        times.push(editor.settled("textDocument/didChange", params)?);
    }
    editor.send(&Json::object([("id", Json::number(2)), ("method", Json::string("shutdown"))]))?;
    editor.send(&Json::object([("method", Json::string("exit"))]))?;
    drop(editor.input);
    let _ = editor.child.wait();
    Ok((median(times), cold))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Budget, Measured, median, passes, table};

    fn measured(enforced: bool, warm: u64) -> Measured {
        let budget = Budget { name: "this", limit: Duration::from_millis(100), enforced };
        Measured { budget, warm: Duration::from_millis(warm), cold: None, counted: None }
    }

    #[test]
    fn only_an_enforced_budget_fails_the_run() {
        assert!(passes(&[measured(true, 100), measured(false, 900)]));
        assert!(!passes(&[measured(true, 101), measured(false, 1)]));
    }

    #[test]
    fn the_table_says_where_each_budget_stands() {
        let table = table(&[measured(true, 1), measured(true, 200), measured(false, 200)], "a size");
        assert!(table.contains("| met |") && table.contains("| **over** |"), "{table}");
        assert!(table.contains("| over, not enforced yet |"), "{table}");
    }

    #[test]
    fn the_growth_line_says_how_much_and_how_much_is_allowed() {
        let line = super::growth_line(9.56, Duration::from_millis(50));
        assert!(
            line.contains("9.6 times") && line.contains("50.0 ms") && line.contains("1.5 times"),
            "{line}"
        );
    }

    #[test]
    fn one_edit_should_run_one_test() {
        assert!(super::ran_one_test("10 passed, 0 failed (1 run, 9 unchanged)\n"));
        assert!(!super::ran_one_test("10 passed, 0 failed (2 run, 8 unchanged)\n"));
        assert!(!super::ran_one_test("10 passed, 0 failed\n"));
    }

    #[test]
    fn a_count_decides_rather_than_the_clock_when_theres_a_count() {
        let counted = |warm, instructions| Measured {
            counted: Some(super::Counted { instructions, most: 100 }),
            ..measured(true, warm)
        };
        assert!(passes(&[counted(900, 100)]), "a slow machine doesn't fail it");
        assert!(!passes(&[counted(1, 101)]), "a fast machine doesn't hide it");
        let table = table(&[counted(900, 100), counted(1, 101)], "a size");
        assert!(table.contains("met by its count, over by the clock"), "{table}");
        assert!(table.contains("**over**"), "{table}");
    }

    #[test]
    fn the_median_is_the_middle_time() {
        let times = [5, 1, 3].map(Duration::from_millis).to_vec();
        assert_eq!(median(times), Duration::from_millis(3));
    }
}
