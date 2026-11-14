//! `fennec dev trace [--json] [path]`: builds a program through LLVM, runs it, and says each place
//! in its source that allocated or copied in the run, how often, how much, and why. The place that
//! did it most comes first, since that's where it costs the most. What the program prints is left
//! out. Blocks the runtime makes are all counted, but what it holds as a `fennec_rt::Value`
//! isn't. Every thread's cost is counted, each place's by the kind of thread it was on: the
//! program's own, a task, a parallel worker or a server's, since a server's cost is in its workers
//! and its tasks rather than in `main`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use fennec_ir::{Cost, Finding};
use fennec_rt::embed::{Counts, RanOn};
use fennec_syntax::Span;

use crate::copies::{bytes, ran};

/// A place in the source that cost something in a run.
struct Place {
    site: u32,
    /// The kind of thread it cost this on.
    ran_on: RanOn,
    counts: Counts,
    why: String,
    loops: usize,
    called_from_loop: Option<String>,
}

impl Place {
    /// What makes a place matter more than another: how many times it cost something.
    fn times(&self) -> u64 {
        self.counts.allocations + self.counts.copies
    }

    /// How often and how much, in words.
    fn cost(&self) -> String {
        let Counts { allocations, allocated_bytes, copies, copied_bytes, .. } = self.counts;
        let mut said = Vec::with_capacity(2);
        if copies > 0 {
            let name = if copies == 1 { "copy" } else { "copies" };
            said.push(format!("{copies} {name} of {}", bytes(copied_bytes)));
        }
        if allocations > 0 {
            let name = if allocations == 1 { "allocation" } else { "allocations" };
            said.push(format!("{allocations} {name} of {}", bytes(allocated_bytes)));
        }
        said.join(", ")
    }

    /// Which kind of thread it cost this on, in words.
    fn on(&self) -> &'static str {
        on(self.ran_on)
    }

    /// Where it sits among loops, in words.
    fn hot(&self) -> String {
        match (self.loops, &self.called_from_loop) {
            (0, None) => String::new(),
            (0, Some(caller)) => format!(" (called from a loop in `{caller}`)"),
            (1, _) => " (in a loop)".to_owned(),
            (loops, _) => format!(" (in {loops} loops)"),
        }
    }
}

/// A kind of thread, in words.
fn on(ran_on: RanOn) -> &'static str {
    match ran_on {
        RanOn::Main => "on main",
        RanOn::Task => "in a task",
        RanOn::Parallel => "on a parallel worker",
        RanOn::Server => "on a server worker",
    }
}

/// Why a place cost what it did: what the compiler works out about it, or else what's there.
fn why(counts: &Counts, found: &[&Finding], what: &str) -> String {
    let copy = found.iter().find(|finding| matches!(finding.cost, Cost::Copy { .. }));
    let allocation = found.iter().find_map(|finding| match &finding.cost {
        Cost::Allocation { what } => Some(what.as_str()),
        Cost::Copy { .. } => None,
    });
    let mut said = Vec::with_capacity(2);
    if counts.copies > 0 {
        said.push(copy.map_or_else(|| format!("{what} copied a shared value"), |copy| copy.message()));
    }
    if counts.allocations > 0 {
        said.push(format!("{} allocated", allocation.unwrap_or(what)));
    }
    said.join("; ")
}

/// Reads what each place cost on each kind of thread, as the run wrote it: the kind's number is
/// last, and a line without one is the program's own thread's.
fn read_sites(text: &str) -> Vec<(u32, RanOn, Counts)> {
    text.lines()
        .filter_map(|line| {
            let mut numbers = line.split(' ').map(str::parse::<u64>);
            let site = u32::try_from(numbers.next()?.ok()?).ok()?;
            let mut next = || numbers.next()?.ok();
            let counts = Counts {
                allocations: next()?,
                allocated_bytes: next()?,
                copies: next()?,
                copied_bytes: next()?,
                largest_copy: next()?,
            };
            let ran_on = match numbers.next() {
                None => RanOn::Main,
                Some(number) => RanOn::from_number(number.ok()?)?,
            };
            Some((site, ran_on, counts))
        })
        .collect()
}

/// The places that cost something, the one that did it most first.
fn places_from(
    costs: Vec<(u32, RanOn, Counts)>,
    sites: &BTreeMap<u32, String>,
    findings: &[Finding],
) -> Vec<Place> {
    let mut by_place: HashMap<u32, Vec<&Finding>> = HashMap::new();
    for finding in findings {
        by_place.entry(finding.span.start).or_default().push(finding);
    }
    let mut places: Vec<Place> = costs
        .into_iter()
        .filter(|(_, _, counts)| counts.allocations + counts.copies > 0)
        .map(|(site, ran_on, counts)| {
            let found = by_place.get(&site).map_or(&[][..], Vec::as_slice);
            let what = sites.get(&site).map_or("this", String::as_str);
            let first = found.first();
            Place {
                site,
                ran_on,
                counts,
                why: why(&counts, found, what),
                loops: first.map_or(0, |finding| finding.loops),
                called_from_loop: first.and_then(|finding| finding.called_from_loop.clone()),
            }
        })
        .collect();
    places.sort_by_key(|place| (std::cmp::Reverse(place.times()), place.site, place.ran_on));
    places
}

/// What a run cost altogether, on the kinds of thread `on` takes.
fn total(places: &[Place], on: impl Fn(RanOn) -> bool) -> Counts {
    let mut all = Counts::default();
    for Place { counts, .. } in places.iter().filter(|place| on(place.ran_on)) {
        all.allocations += counts.allocations;
        all.allocated_bytes += counts.allocated_bytes;
        all.copies += counts.copies;
        all.copied_bytes += counts.copied_bytes;
        all.largest_copy = all.largest_copy.max(counts.largest_copy);
    }
    all
}

/// The kinds of thread something cost on, in order.
fn kinds(places: &[Place]) -> Vec<RanOn> {
    let mut kinds: Vec<RanOn> = places.iter().map(|place| place.ran_on).collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// A run of a program built to trace itself.
struct Run {
    program: fennec_kit::Program,
    places: Vec<Place>,
    /// What it said if it failed.
    failure: Option<String>,
}

/// Builds and runs the program at `path`, in `work`.
fn traced_run(path: &str, work: &Path) -> Result<Run, ExitCode> {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return Err(ExitCode::FAILURE) };
    if !crate::run::check(&mut program) {
        return Err(ExitCode::FAILURE);
    }
    let executable = work.join(format!("traced{}", std::env::consts::EXE_SUFFIX));
    let file = work.join("sites");
    let (sites, lowered) = crate::aot::tracing_executable(&program, &executable).map_err(|why| {
        eprintln!("error: only what LLVM can build can be traced, and it can't build this yet: {why}");
        ExitCode::FAILURE
    })?;
    let ran = Command::new(&executable)
        .env("FENNEC_TRACE_FILE", &file)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            eprintln!("error: couldn't run the program: {err}");
            ExitCode::FAILURE
        })?;
    let failure = (!ran.status.success()).then(|| String::from_utf8_lossy(&ran.stderr).into_owned());
    let written = std::fs::read_to_string(&file).unwrap_or_default();
    let places = places_from(read_sites(&written), &sites, &fennec_ir::costs(&lowered));
    Ok(Run { program, places, failure })
}

/// Runs `report` on a traced run in a folder of its own, which is gone afterwards.
fn with_run(path: &str, report: fn(&Run)) -> ExitCode {
    let work = crate::own_temp("trace");
    if let Err(err) = std::fs::create_dir_all(&work) {
        eprintln!("error: couldn't make {}: {err}", work.display());
        return ExitCode::FAILURE;
    }
    let run = traced_run(path, &work);
    let _ = std::fs::remove_dir_all(&work);
    match run {
        Ok(run) => {
            report(&run);
            if let Some(failure) = &run.failure {
                eprint!("{failure}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

/// What `fennec dev trace` was asked to do, from what follows it on the command line.
pub(crate) fn command(rest: &[&str]) -> ExitCode {
    match rest {
        [] => crate::on_big_stack(human, "."),
        ["--json"] => crate::on_big_stack(json, "."),
        ["--json", path] => crate::on_big_stack(json, path),
        [path] => crate::on_big_stack(human, path),
        _ => {
            eprintln!("usage: fennec dev trace [--json] [path]");
            ExitCode::FAILURE
        }
    }
}

fn human(path: &str) -> ExitCode {
    with_run(path, |run| {
        for place in &run.places {
            let span = Span { start: place.site, end: place.site };
            let (file, line, column, text) = run.program.sources.locate(span).unwrap_or(("?", 0, 0, ""));
            println!(
                "{file}:{line}:{column}{}, {}: {}: {}",
                place.hot(),
                place.on(),
                place.cost(),
                place.why
            );
            println!("    {}", text.trim());
        }
        println!("{}", ran(&total(&run.places, |_| true)));
        // Where it went, once anything but the program's own thread cost something.
        if run.places.iter().any(|place| place.ran_on != RanOn::Main) {
            for kind in kinds(&run.places) {
                let counts = total(&run.places, |ran_on| ran_on == kind);
                println!("  {}", ran(&counts).replacen("in a run", on(kind), 1));
            }
        }
    })
}

fn json(path: &str) -> ExitCode {
    with_run(path, |run| {
        use fennec_lsp::Json;
        let number = |count: u64| Json::number(usize::try_from(count).unwrap_or(usize::MAX));
        let places = run.places.iter().map(|place| {
            let span = Span { start: place.site, end: place.site };
            let (file, line, column, _) = run.program.sources.locate(span).unwrap_or(("?", 0, 0, ""));
            Json::object([
                ("file", Json::string(file)),
                ("line", Json::number(line)),
                ("column", Json::number(column)),
                ("allocations", number(place.counts.allocations)),
                ("allocated_bytes", number(place.counts.allocated_bytes)),
                ("copies", number(place.counts.copies)),
                ("copied_bytes", number(place.counts.copied_bytes)),
                ("largest_copy", number(place.counts.largest_copy)),
                ("ran_on", Json::string(place.ran_on.name())),
                ("why", Json::string(&place.why)),
                ("loops", Json::number(place.loops)),
                ("called_from_loop", place.called_from_loop.as_deref().map_or(Json::Null, Json::string)),
            ])
        });
        let all = total(&run.places, |_| true);
        let by_thread = kinds(&run.places)
            .into_iter()
            .map(|kind| {
                let counts = total(&run.places, |ran_on| ran_on == kind);
                let counted = Json::object([
                    ("allocations", number(counts.allocations)),
                    ("allocated_bytes", number(counts.allocated_bytes)),
                    ("copies", number(counts.copies)),
                    ("copied_bytes", number(counts.copied_bytes)),
                ]);
                (kind.name().to_owned(), counted)
            })
            .collect();
        let report = Json::object([
            ("places", Json::Array(places.collect())),
            ("allocations", number(all.allocations)),
            ("allocated_bytes", number(all.allocated_bytes)),
            ("copies", number(all.copies)),
            ("copied_bytes", number(all.copied_bytes)),
            ("by_thread", Json::Object(by_thread)),
            ("failed", Json::Bool(run.failure.is_some())),
        ]);
        println!("{report}");
    })
}

#[cfg(test)]
mod tests {
    use fennec_rt::embed::{Counts, RanOn};

    use super::read_sites;

    #[test]
    fn it_reads_what_a_run_wrote() {
        let wanted =
            Counts { allocations: 2, allocated_bytes: 80, copies: 3, copied_bytes: 24, largest_copy: 8 };
        assert_eq!(read_sites("7 2 80 3 24 8\nnot numbers\n9 1\n"), [(7, RanOn::Main, wanted)]);
    }

    /// Each place's kind of thread is its line's last number, and a number that's no kind is
    /// passed over rather than taken for one.
    #[test]
    fn it_reads_which_kind_of_thread_each_cost_was_on() {
        let one = Counts { allocations: 1, allocated_bytes: 8, ..Counts::default() };
        assert_eq!(
            read_sites("7 1 8 0 0 0 0\n7 1 8 0 0 0 1\n8 1 8 0 0 0 2\n9 1 8 0 0 0 3\n9 1 8 0 0 0 4\n"),
            [
                (7, RanOn::Main, one),
                (7, RanOn::Task, one),
                (8, RanOn::Parallel, one),
                (9, RanOn::Server, one)
            ]
        );
    }
}
