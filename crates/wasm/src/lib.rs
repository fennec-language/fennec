//! Fennec's interpreter as a WebAssembly module, for the playground on the website.
//!
//! The page talks to the module through its memory, with no bindings library:
//!
//! 1. `fennec_input(len)` makes room for `len` bytes of UTF-8 source and returns where they go.
//! 2. The page writes the source there, then calls `fennec_run()` or `fennec_test()`, which
//!    returns the length of the result.
//! 3. `fennec_output()` returns where the result starts. It's JSON:
//!    `{"ok": true, "parts": [["out", "Hello\n"], ["err", "error: ..."]]}`, where `out` is what
//!    the CLI prints to stdout and `err` what it prints to stderr.
//!
//! Programs get no files, each run stops after [`STEP_LIMIT`] steps, and calls nest at most
//! [`DEPTH_LIMIT`] deep.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write;

use fennec_interp::Interp;
use fennec_syntax::{Diagnostic, Module};

/// How many steps a run, or each test, may take: a few seconds' work in a browser.
pub const STEP_LIMIT: u64 = 50_000_000;

/// How deep calls may nest. Browsers give WebAssembly far less stack than the CLI gives itself:
/// in Chrome's workers, plain recursion runs out at about 240 nested Fennec calls, and sooner
/// when each call nests deeper expressions. The page reports running out regardless.
pub const DEPTH_LIMIT: usize = 150;

/// How much a program may print before the rest is dropped, so a runaway `print` can't bury
/// the page.
pub const OUTPUT_LIMIT: usize = 1024 * 1024;

/// The name errors show for the program, where the CLI shows its path.
const PATH: &str = "playground.fen";

/// Which of the CLI's streams a part of the result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// What running a program printed, and whether it succeeded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub ok: bool,
    pub parts: Vec<(Stream, String)>,
}

impl Report {
    fn out(&mut self, text: impl Into<String>) {
        self.push(Stream::Out, text.into());
    }

    fn err(&mut self, text: impl Into<String>) {
        self.push(Stream::Err, text.into());
    }

    /// Adds text, joining it to the last part when that's the same stream.
    fn push(&mut self, stream: Stream, text: String) {
        if text.is_empty() {
            return;
        }
        match self.parts.last_mut() {
            Some((last, joined)) if *last == stream => joined.push_str(&text),
            _ => self.parts.push((stream, text)),
        }
    }

    fn failed(mut self, source: &str, diagnostic: &Diagnostic) -> Self {
        self.err(diagnostic.render(PATH, source));
        self.ok = false;
        self
    }

    pub fn to_json(&self) -> String {
        let mut json = format!("{{\"ok\":{},\"parts\":[", self.ok);
        for (at, (stream, text)) in self.parts.iter().enumerate() {
            let stream = match stream {
                Stream::Out => "out",
                Stream::Err => "err",
            };
            let comma = if at == 0 { "" } else { "," };
            let _ = write!(json, "{comma}[\"{stream}\",{}]", json_string(text));
        }
        json.push_str("]}");
        json
    }
}

fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for ch in text.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            ch if u32::from(ch) < 0x20 => {
                let _ = write!(quoted, "\\u{:04x}", u32::from(ch));
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// Collects output up to [`OUTPUT_LIMIT`] bytes and quietly drops the rest.
#[derive(Default)]
struct Capped {
    bytes: Vec<u8>,
    cut: bool,
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = OUTPUT_LIMIT.saturating_sub(self.bytes.len());
        self.bytes.extend(buf.iter().take(room));
        self.cut |= buf.len() > room;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capped {
    fn into_report(self, report: &mut Report) {
        report.out(String::from_utf8_lossy(&self.bytes));
        if self.cut {
            report.err(format!("note: output stopped after {} KiB\n", OUTPUT_LIMIT / 1024));
        }
    }
}

/// Parses and type-checks a program, as the CLI does before running anything.
fn checked(source: &str) -> Result<Module, Report> {
    let module =
        fennec_syntax::parse(source).map_err(|diagnostic| Report::default().failed(source, &diagnostic))?;
    fennec_check::check(&module).map_err(|diagnostics| {
        let rendered: Vec<String> =
            diagnostics.iter().map(|diagnostic| diagnostic.render(PATH, source)).collect();
        let mut report = Report::default();
        report.err(rendered.join("\n"));
        report
    })?;
    Ok(module)
}

fn interp(out: &mut dyn Write, steps: u64) -> Result<Interp<'_>, Diagnostic> {
    let mut interp = Interp::new(out)?;
    interp.limit_steps(steps);
    interp.limit_depth(DEPTH_LIMIT);
    interp.deny_files();
    Ok(interp)
}

/// Runs a program's `main`, like `fennec run`.
pub fn run(source: &str) -> Report {
    run_within(source, STEP_LIMIT)
}

/// Runs a program's `test` blocks, like `fennec test`.
pub fn test(source: &str) -> Report {
    test_within(source, STEP_LIMIT)
}

fn run_within(source: &str, steps: u64) -> Report {
    let module = match checked(source) {
        Ok(module) => module,
        Err(report) => return report,
    };
    let report = Report::default();
    let mut out = Capped::default();
    let result = interp(&mut out, steps).and_then(|mut interp| interp.run_program(&module));
    let mut report = Report { ok: true, ..report };
    out.into_report(&mut report);
    match result {
        Ok(()) => report,
        Err(diagnostic) => report.failed(source, &diagnostic),
    }
}

fn test_within(source: &str, steps: u64) -> Report {
    let module = match checked(source) {
        Ok(module) => module,
        Err(report) => return report,
    };
    let report = Report::default();
    let mut out = std::io::sink();
    let mut interp = match interp(&mut out, steps) {
        Ok(interp) => interp,
        Err(diagnostic) => return report.failed(source, &diagnostic),
    };
    if let Err(diagnostic) = interp.load(&module) {
        return report.failed(source, &diagnostic);
    }
    let outcomes = interp.run_tests();
    let mut report = Report { ok: true, ..report };
    if outcomes.is_empty() {
        report.out(format!("no tests in {PATH}\n"));
        return report;
    }
    let mut failed = 0;
    for outcome in &outcomes {
        let cases = if outcome.cases > 1 { format!(" ({} inputs)", outcome.cases) } else { String::new() };
        match &outcome.failure {
            None => report.out(format!("  ✓ {}{cases}\n", outcome.name)),
            Some(diagnostic) => {
                failed += 1;
                report.out(format!("  ✗ {}\n", outcome.name));
                report.err(diagnostic.render(PATH, source));
            }
        }
    }
    let passed = outcomes.len() - failed;
    report.out(format!("{passed} passed, {failed} failed\n"));
    report.ok = failed == 0;
    report
}

thread_local! {
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Makes room for `len` bytes of source and returns their address, for the page to fill in.
#[expect(unsafe_code, reason = "the page calls this by name, so it must keep its name")]
#[unsafe(no_mangle)]
pub extern "C" fn fennec_input(len: usize) -> usize {
    INPUT.with_borrow_mut(|input| {
        input.clear();
        input.resize(len, 0);
        input.as_mut_ptr().expose_provenance()
    })
}

/// Runs the source as a program and returns the length of the result.
#[expect(unsafe_code, reason = "the page calls this by name, so it must keep its name")]
#[unsafe(no_mangle)]
pub extern "C" fn fennec_run() -> usize {
    respond(run)
}

/// Runs the source's tests and returns the length of the result.
#[expect(unsafe_code, reason = "the page calls this by name, so it must keep its name")]
#[unsafe(no_mangle)]
pub extern "C" fn fennec_test() -> usize {
    respond(test)
}

/// The address of the last result.
#[expect(unsafe_code, reason = "the page calls this by name, so it must keep its name")]
#[unsafe(no_mangle)]
pub extern "C" fn fennec_output() -> usize {
    OUTPUT.with_borrow(|output| output.as_ptr().expose_provenance())
}

fn respond(action: fn(&str) -> Report) -> usize {
    let source = INPUT.with_borrow(|input| String::from_utf8_lossy(input).into_owned());
    let json = action(&source).to_json().into_bytes();
    let len = json.len();
    OUTPUT.set(json);
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enough for the tests' programs, and quick to run out of.
    const STEPS: u64 = 100_000;

    /// Runs `work` on its own thread and fails if it's still going after ten seconds. Some
    /// tests run programs that never end on their own, so a broken limit should fail them
    /// quickly rather than hang the run.
    fn within_deadline<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(work()));
        result.recv_timeout(std::time::Duration::from_secs(10)).expect("still running after ten seconds")
    }

    const HELLO: &str = "def main(stdout: Stdout):\n    stdout.print(\"Hello, Fennec\")\n";

    #[test]
    fn runs_a_program() {
        let report = run(HELLO);
        assert_eq!(report, Report { ok: true, parts: vec![(Stream::Out, "Hello, Fennec\n".into())] });
        assert_eq!(report.to_json(), r#"{"ok":true,"parts":[["out","Hello, Fennec\n"]]}"#);
    }

    #[test]
    fn renders_errors_like_the_cli() {
        let report = run("def main(stdout: Stdout):\n    x = 1\n    x = 2\n");
        assert!(!report.ok);
        let (stream, text) = &report.parts[0];
        assert_eq!(*stream, Stream::Err);
        assert!(text.starts_with("error: "), "{text}");
        assert!(text.contains("--> playground.fen:3:"), "{text}");
    }

    #[test]
    fn stops_a_loop_that_never_ends() {
        let report = within_deadline(|| {
            run_within(
                "def main(stdout: Stdout):\n    stdout.print(\"before\")\n    while True:\n        pass\n",
                STEPS,
            )
        });
        assert!(!report.ok);
        assert_eq!(report.parts[0], (Stream::Out, "before\n".into()));
        let (_, error) = &report.parts[1];
        assert!(error.starts_with(&format!("error: stopped after {STEPS} steps")), "{error}");
        assert!(error.contains("loop that never ends"), "{error}");
    }

    #[test]
    fn stops_a_test_that_never_ends() {
        let report = within_deadline(|| {
            test_within("test \"spins\" for n: int:\n    while True:\n        pass\n", STEPS)
        });
        assert!(!report.ok);
        assert_eq!(report.parts[0], (Stream::Out, "  ✗ spins\n".into()));
        assert!(report.parts[1].1.contains("stopped after"), "{:?}", report.parts);
    }

    #[test]
    fn limits_recursion() {
        let source = "def down(n: int) -> int:\n    return down(n + 1)\n\ndef main(stdout: Stdout):\n    stdout.print(down(0))\n";
        // Unoptimized native code needs far more stack per call than the release module does.
        let runner = std::thread::Builder::new().stack_size(256 * 1024 * 1024);
        let report = runner.spawn(move || run_within(source, STEPS)).unwrap().join().unwrap();
        assert!(report.parts[0].1.starts_with("error: calls nested too deeply"), "{:?}", report.parts);
    }

    #[test]
    fn has_no_files() {
        let report = run("def main(stdout: Stdout, fs: Fs):\n    pass\n");
        assert!(!report.ok);
        assert!(report.parts[0].1.contains("there are no files"), "{:?}", report.parts);
    }

    #[test]
    fn caps_output() {
        let line = "spam ".repeat(100);
        let source =
            format!("def main(stdout: Stdout):\n    while True:\n        stdout.print(\"{line}\")\n");
        let report = within_deadline(move || run_within(&source, STEPS));
        assert_eq!(report.parts[0].1.len(), 1024 * 1024);
        assert!(
            report.parts[1].1.starts_with("note: output stopped after 1024 KiB\nerror: "),
            "{:?}",
            report.parts[1]
        );
    }

    #[test]
    fn output_that_exactly_fits_isnt_cut() {
        let mut out = Capped::default();
        out.write_all(&vec![b'x'; OUTPUT_LIMIT]).unwrap();
        assert!(!out.cut);
        out.write_all(b"y").unwrap();
        assert!(out.cut);
        assert_eq!(out.bytes.len(), OUTPUT_LIMIT);
    }

    #[test]
    fn type_checks_before_running() {
        let source = "def area(width: float, height: float):\n    width * height\n\ndef main(stdout: Stdout):\n    stdout.print(\"ran\")\n    stdout.print(area(width=2.0))\n";
        let report = run(source);
        assert!(!report.ok);
        // Nothing ran: the checker stopped it, so "ran" was never printed.
        assert_eq!(report.parts.len(), 1, "{:?}", report.parts);
        let (stream, error) = &report.parts[0];
        assert_eq!(*stream, Stream::Err);
        assert!(error.starts_with("error: `area` is missing its `height` argument\n"), "{error}");
        assert!(error.contains("--> playground.fen:6:18"), "{error}");
    }

    #[test]
    fn reports_passing_tests() {
        let source = "test \"adds\":\n    assert 1 + 1 == 2\n\ntest \"any\" for n: int:\n    assert n == n\n";
        let report = test(source);
        let expected = "  ✓ adds\n  ✓ any (100 inputs)\n2 passed, 0 failed\n";
        assert_eq!(report, Report { ok: true, parts: vec![(Stream::Out, expected.into())] });
        let empty = test(HELLO);
        assert_eq!(
            empty,
            Report { ok: true, parts: vec![(Stream::Out, "no tests in playground.fen\n".into())] }
        );
    }

    #[test]
    fn runs_tests_like_the_cli() {
        let source = "test \"adds\":\n    assert 1 + 1 == 2\n\ntest \"breaks\":\n    assert 1 == 2\n";
        let report = test(source);
        assert!(!report.ok);
        assert_eq!(report.parts[0], (Stream::Out, "  ✓ adds\n  ✗ breaks\n".into()));
        assert!(report.parts[1].1.contains("--> playground.fen:5:"), "{:?}", report.parts);
        assert_eq!(report.parts[2], (Stream::Out, "1 passed, 1 failed\n".into()));
    }

    #[test]
    fn talks_through_memory() {
        let at = fennec_input(HELLO.len());
        assert_ne!(at, 0);
        INPUT.with_borrow_mut(|input| input.copy_from_slice(HELLO.as_bytes()));
        let len = fennec_run();
        let json = OUTPUT.with_borrow(|output| String::from_utf8(output.clone()).unwrap());
        assert_eq!(json.len(), len);
        assert!(json.contains("Hello, Fennec"));
        assert_eq!(fennec_output(), OUTPUT.with_borrow(|output| output.as_ptr().addr()));
        let len = fennec_test();
        let json = OUTPUT.with_borrow(|output| String::from_utf8(output.clone()).unwrap());
        assert_eq!(json.len(), len);
        assert!(json.contains("no tests"), "{json}");
    }

    #[test]
    fn escapes_json() {
        assert_eq!(json_string("a\"b\\c\n\u{1}"), r#""a\"b\\c\n\u0001""#);
    }
}
