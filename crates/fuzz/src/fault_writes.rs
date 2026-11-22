//! The `fault-writes` target: a program writes two files over old ones, whole and through `with
//! fs.create(...)` blocks, while the runtime's fault hook (`FENNEC_FAULTS`) fails its work on the
//! disk partway: a full disk, a quota, a read-only disk, a refusal, a file too big, an I/O error,
//! or a kill. Each run's output, and what's on the disk after, must be what the model here says:
//! each target the old file or the new one, byte for byte, never torn, unless a block caught a
//! failed write and went on, which keeps what landed; and a file of the write's own left beside
//! its target only where a kill came between making it and putting it in place.
//!
//! The model follows the hook's agreement with the runtime lane, in the coverage lane's notes as
//! `runtime-lane-fault-hook-spec.md`: what each call counts, in order, and what each failure leaves,
//! with the write-safety change of #2271. A whole write syncs the file before its rename and the
//! folder after, unless it's `durable=False`; a refusal to make the file beside its target fails
//! the write, leaving the target as it was; and a run's first write into a folder clears the files
//! left beside their targets by runs that are gone, but not those of a run that's still going.
//!
//! The input chooses the program, one to three writes, each whole (text or bytes, durable or not)
//! or a block of one to five pieces: writes, lines, `flush()` and `sync()`, some catching their
//! own failure. It then
//! chooses four sets of rules, each failing one or two pieces of the work a run without rules
//! does, or work past it. Every program runs without rules first, then with each set, built as a
//! dev build and with `--release`.
//!
//! It needs `fennec` beside a runtime built with the `faults` feature, named by
//! `FENNEC_FAULTED`, or `fennec` itself when that's left out. A runtime that ships reads no rules,
//! so a canary run first must fail where its rule says, and the target stops if it doesn't.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Instant;

use crate::supervise::{Failure, MOST_KILOBYTES, STUCK, watch};

/// The two files each program writes, and what each holds before a run.
const FILES: [(&str, &str); 2] = [("f.txt", "old f\nkept whole\n"), ("g.txt", "old g\n")];

/// The characters a written text is made of: letters, digits, a space, a letter past ASCII and an
/// emoji, so a write cut short can end partway through a character.
const CHARACTERS: [&str; 12] = ["a", "b", "c", "x", "y", "z", "0", "1", " ", "-", "\u{e9}", "\u{1f98a}"];

/// How many sets of rules each program runs with.
const FAULTED_RUNS: usize = 4;

/// Work on the disk the hook counts, each kind from 1 over a whole run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Work {
    Create,
    Write,
    Flush,
    Sync,
    Rename,
}

const WORKS: [Work; 5] = [Work::Create, Work::Write, Work::Flush, Work::Sync, Work::Rename];

impl Work {
    fn name(self) -> &'static str {
        match self {
            Work::Create => "create",
            Work::Write => "write",
            Work::Flush => "flush",
            Work::Sync => "sync",
            Work::Rename => "rename",
        }
    }

    fn at(self) -> usize {
        match self {
            Work::Create => 0,
            Work::Write => 1,
            Work::Flush => 2,
            Work::Sync => 3,
            Work::Rename => 4,
        }
    }
}

/// What a rule puts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    NoSpace,
    Quota,
    ReadOnly,
    Denied,
    TooLarge,
    Io,
    /// The process ends there at once, as `SIGKILL` ends it.
    Kill,
}

const FAULTS: [Fault; 7] =
    [Fault::NoSpace, Fault::Quota, Fault::ReadOnly, Fault::Denied, Fault::TooLarge, Fault::Io, Fault::Kill];

impl Fault {
    fn name(self) -> &'static str {
        match self {
            Fault::NoSpace => "nospace",
            Fault::Quota => "quota",
            Fault::ReadOnly => "readonly",
            Fault::Denied => "denied",
            Fault::TooLarge => "toolarge",
            Fault::Io => "io",
            Fault::Kill => "kill",
        }
    }

    /// The failure as a program prints it: an `IoError`'s words. A refusal says the path it
    /// was at, when the work was done for a call on `fs` given one, and not for a file's own.
    fn printed(self, at: Option<&str>) -> String {
        let said = match self {
            Fault::NoSpace => "the disk is full",
            Fault::Quota => "the disk space allowed is used up",
            Fault::ReadOnly => "the disk can't be written to",
            Fault::Denied => "the system won't allow it",
            Fault::TooLarge => "the file is too big for the system",
            Fault::Io => "the system failed: other error",
            Fault::Kill => "",
        };
        match (self, at) {
            (Fault::Denied, Some(path)) => format!("`{path}`: {said}"),
            _ => said.to_owned(),
        }
    }
}

/// A rule: the `n`th of `work` fails with `fault`, a write letting `bytes` of its own through
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rule {
    work: Work,
    n: usize,
    fault: Fault,
    bytes: usize,
}

/// `rules` as `FENNEC_FAULTS` takes them.
fn spec(rules: &[Rule]) -> String {
    let each: Vec<String> = rules
        .iter()
        .map(|rule| {
            let base = format!("{}:{}:{}", rule.work.name(), rule.n, rule.fault.name());
            if rule.work == Work::Write { format!("{base}:{}", rule.bytes) } else { base }
        })
        .collect();
    each.join(",")
}

/// A piece of a block.
#[derive(Clone, Debug)]
enum Piece {
    /// `file.write_text(text)`, or `write_bytes(text.encode())`, its failure caught there or
    /// passed up out of the block.
    Write { text: String, bytes: bool, caught: bool },
    /// `file.write_line(text)`: two writes, the text then its line's end.
    Line { text: String, caught: bool },
    /// `file.flush()?`.
    Flush,
    /// `file.sync()?`: a flush, then a sync.
    Sync,
}

/// One of a program's writes.
#[derive(Clone, Debug)]
enum Act {
    /// `fs.write_text(name, text)?`, or `write_bytes(name, text.encode())?`, with
    /// `durable=False` where it isn't durable.
    Whole { file: usize, text: String, bytes: bool, durable: bool },
    /// `with fs.create(name)? as file:`, with `durable=False` where it isn't durable.
    Block { file: usize, durable: bool, pieces: Vec<Piece> },
}

/// What the input chooses.
struct Plan {
    acts: Vec<Act>,
    /// The bytes each set of rules is chosen from, once the work a run without rules does is known.
    choices: Vec<u8>,
}

/// Bytes read one at a time, going on with a stream seeded by them once they run out.
struct Bytes<'a> {
    input: &'a [u8],
    at: usize,
    state: u64,
}

impl Bytes<'_> {
    fn new(input: &[u8]) -> Bytes<'_> {
        let state = input.iter().fold(0x9e37_79b9_7f4a_7c15_u64, |state, byte| {
            (state ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
        Bytes { input, at: 0, state }
    }

    fn byte(&mut self) -> u8 {
        let got = self.input.get(self.at).copied();
        self.at += 1;
        got.unwrap_or_else(|| {
            self.state =
                self.state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            self.state.to_le_bytes().get(5).copied().unwrap_or(0)
        })
    }

    /// A number below `n`.
    fn below(&mut self, n: usize) -> usize {
        usize::from(self.byte()) % n.max(1)
    }

    fn text(&mut self) -> String {
        let length = 1 + self.below(12);
        (0..length).map(|_| CHARACTERS.get(self.below(CHARACTERS.len())).copied().unwrap_or("a")).collect()
    }
}

fn plan(input: &[u8]) -> Plan {
    let mut bytes = Bytes::new(input);
    let mut acts = Vec::new();
    for _ in 0..=bytes.below(3) {
        let file = bytes.below(2);
        acts.push(if bytes.below(2) == 0 {
            Act::Whole { file, text: bytes.text(), bytes: bytes.below(2) == 0, durable: bytes.below(3) != 0 }
        } else {
            let durable = bytes.below(3) != 0;
            let mut pieces = Vec::new();
            for _ in 0..=bytes.below(5) {
                pieces.push(match bytes.below(6) {
                    0 | 1 => Piece::Write {
                        text: bytes.text(),
                        bytes: bytes.below(2) == 0,
                        caught: bytes.below(3) == 0,
                    },
                    2 | 3 => Piece::Line { text: bytes.text(), caught: bytes.below(3) == 0 },
                    4 => Piece::Flush,
                    _ => Piece::Sync,
                });
            }
            Act::Block { file, durable, pieces }
        });
    }
    let choices = (0..FAULTED_RUNS * 8).map(|_| bytes.byte()).collect();
    Plan { acts, choices }
}

/// `text` as a Fennec string literal.
fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The name of file `file`.
fn file_name(file: usize) -> &'static str {
    FILES.get(file).map_or("f.txt", |(name, _)| name)
}

fn source(plan: &Plan) -> String {
    let mut out = String::new();
    for (at, act) in plan.acts.iter().enumerate() {
        let _ = writeln!(out, "def act{at}(fs: Fs, stdout: Stdout) fails IoError:");
        match act {
            Act::Whole { file, text, bytes, durable } => {
                let (name, text) = (file_name(*file), literal(text));
                let durable = if *durable { "" } else { ", durable=False" };
                let _ = if *bytes {
                    writeln!(out, "    fs.write_bytes(\"{name}\", {text}.encode(){durable})?")
                } else {
                    writeln!(out, "    fs.write_text(\"{name}\", {text}{durable})?")
                };
                let _ = writeln!(out, "    stdout.print(\"wrote\", {at})\n");
            }
            Act::Block { file, durable, pieces } => {
                let durable = if *durable { "" } else { ", durable=False" };
                let _ = writeln!(out, "    with fs.create(\"{}\"{durable})? as file:", file_name(*file));
                for piece in pieces {
                    let (call, caught) = match piece {
                        Piece::Write { text, bytes: true, caught } => {
                            (format!("file.write_bytes({}.encode())", literal(text)), *caught)
                        }
                        Piece::Write { text, bytes: false, caught } => {
                            (format!("file.write_text({})", literal(text)), *caught)
                        }
                        Piece::Line { text, caught } => {
                            (format!("file.write_line({})", literal(text)), *caught)
                        }
                        Piece::Flush => ("file.flush()".to_owned(), false),
                        Piece::Sync => ("file.sync()".to_owned(), false),
                    };
                    let _ = if caught {
                        writeln!(out, "        {call} except why:\n            stdout.print(\"caught\", why)")
                    } else {
                        writeln!(out, "        {call}?")
                    };
                }
                let _ = writeln!(out, "    stdout.print(\"closed\", {at})\n");
            }
        }
    }
    out.push_str("def main(stdout: Stdout, fs: Fs):\n");
    for at in 0..plan.acts.len() {
        let _ = writeln!(out, "    act{at}(fs, stdout) except why:\n        stdout.print(\"failed\", why)");
    }
    out.push_str("    stdout.print(\"done\")\n");
    out
}

/// The program the input makes, then the rules of each run, for `fennec-fuzz show fault-writes`.
pub(crate) fn written(input: &[u8]) -> String {
    let plan = plan(input);
    let mut out = source(&plan);
    for rules in rule_sets(&plan) {
        let _ = writeln!(out, "# FENNEC_FAULTS={}", spec(&rules));
    }
    out
}

/// What a run leaves: what it printed, unless it was killed, which may lose what it held back to
/// print; each file, or none; and the file of a write's own left beside its target, if one is.
#[derive(Debug, PartialEq, Eq)]
struct Left {
    printed: Option<String>,
    killed: bool,
    files: [Option<Vec<u8>>; 2],
    partial: Option<(usize, Vec<u8>)>,
}

/// A run of the model: the rules, how much of each kind of work it's begun, and each piece of work
/// with its length, for a write.
struct Model<'a> {
    rules: &'a [Rule],
    counts: [usize; 5],
    work: Vec<(Work, usize)>,
}

/// How a piece of work went.
enum Went {
    Done,
    /// A failure a program sees, the write letting this many of its bytes through first.
    Failed(Fault, usize),
    /// A kill, the write letting this many of its bytes through first.
    Killed(usize),
}

impl Model<'_> {
    /// Begins `work`, of `length` bytes for a write.
    fn begin(&mut self, work: Work, length: usize) -> Went {
        let count = self.counts.get_mut(work.at()).map_or(0, |count| {
            *count += 1;
            *count
        });
        self.work.push((work, length));
        match self.rules.iter().find(|rule| rule.work == work && rule.n == count) {
            None => Went::Done,
            Some(rule) if rule.fault == Fault::Kill => Went::Killed(rule.bytes.min(length)),
            Some(rule) => Went::Failed(rule.fault, rule.bytes.min(length)),
        }
    }
}

/// How an act ended, for the rest of the run.
enum Ended {
    /// It printed this line, and the run goes on.
    Printed(String),
    /// The process was killed, leaving this file of a write's own beside its target, if any.
    Killed(Option<(usize, Vec<u8>)>),
}

/// What a run of `plan` under `rules` leaves, and the work it did.
fn modelled(plan: &Plan, rules: &[Rule]) -> (Left, Vec<(Work, usize)>) {
    let mut model = Model { rules, counts: [0; 5], work: Vec::new() };
    let mut files: [Option<Vec<u8>>; 2] = FILES.map(|(_, old)| Some(old.as_bytes().to_vec()));
    let mut printed = String::new();
    for (at, act) in plan.acts.iter().enumerate() {
        let ended = match act {
            Act::Whole { file, text, durable, .. } => {
                whole(&mut model, &mut files, (*file, text.as_bytes(), *durable), at)
            }
            Act::Block { file, durable, pieces } => {
                block(&mut model, &mut files, (*file, *durable), pieces, (at, &mut printed))
            }
        };
        match ended {
            Ended::Printed(line) => printed.push_str(&line),
            Ended::Killed(partial) => {
                return (Left { printed: None, killed: true, files, partial }, model.work);
            }
        }
    }
    printed.push_str("done\n");
    (Left { printed: Some(printed), killed: false, files, partial: None }, model.work)
}

/// A run that ends failing with `fault`, printed with the path it was at, if any.
fn failed(fault: Fault, at: Option<&str>) -> Ended {
    Ended::Printed(format!("failed {}\n", fault.printed(at)))
}

/// `fs.write_text` or `fs.write_bytes`: create, write, a sync of the file where it's durable,
/// rename, and a sync of the folder where it's durable. A failure before the rename throws its file
/// away, leaving the target as it was, a refusal to make it too; the folder's sync fails only
/// after the rename.
fn whole(
    model: &mut Model<'_>,
    files: &mut [Option<Vec<u8>>; 2],
    (file, data, durable): (usize, &[u8], bool),
    at: usize,
) -> Ended {
    // Each step is `fs.write_text`'s or `fs.write_bytes`'s, given the file's path.
    let path = Some(file_name(file));
    match model.begin(Work::Create, 0) {
        Went::Done => {}
        Went::Killed(_) => return Ended::Killed(None),
        Went::Failed(fault, _) => return failed(fault, path),
    }
    match model.begin(Work::Write, data.len()) {
        Went::Done => {}
        Went::Killed(landed) => {
            return Ended::Killed(Some((file, data.get(..landed).unwrap_or_default().to_vec())));
        }
        Went::Failed(fault, _) => return failed(fault, path),
    }
    if durable {
        match model.begin(Work::Sync, 0) {
            Went::Done => {}
            Went::Killed(_) => return Ended::Killed(Some((file, data.to_vec()))),
            Went::Failed(fault, _) => return failed(fault, path),
        }
    }
    match model.begin(Work::Rename, 0) {
        Went::Done => {}
        Went::Killed(_) => return Ended::Killed(Some((file, data.to_vec()))),
        Went::Failed(fault, _) => return failed(fault, path),
    }
    if let Some(target) = files.get_mut(file) {
        *target = Some(data.to_vec());
    }
    if durable {
        match model.begin(Work::Sync, 0) {
            Went::Done => {}
            Went::Killed(_) => return Ended::Killed(None),
            Went::Failed(fault, _) => return failed(fault, path),
        }
    }
    Ended::Printed(format!("wrote {at}\n"))
}

/// A `with fs.create(...)` block: create; each piece's work, what's written held back until a
/// flush; then at its end a flush, a sync where it's durable, the rename, and the folder's sync
/// where it's durable. A failure leaving the block throws its file away, leaving the target as it
/// was, and the folder's sync fails only after the rename. A kill leaves the file as it was on the
/// disk, which a write's kill flushes first.
fn block(
    model: &mut Model<'_>,
    files: &mut [Option<Vec<u8>>; 2],
    (file, durable): (usize, bool),
    pieces: &[Piece],
    (at, printed): (usize, &mut String),
) -> Ended {
    // `fs.create` is given the file's path; each step past it is the file's own, which has none.
    match model.begin(Work::Create, 0) {
        Went::Done => {}
        Went::Killed(_) => return Ended::Killed(None),
        Went::Failed(fault, _) => return failed(fault, Some(file_name(file))),
    }
    // Everything written, and how much of it a flush has put on the disk.
    let (mut written, mut landed) = (Vec::<u8>::new(), 0);
    for piece in pieces {
        let (writes, caught): (Vec<&[u8]>, bool) = match piece {
            Piece::Write { text, caught, .. } => (vec![text.as_bytes()], *caught),
            Piece::Line { text, caught } => (vec![text.as_bytes(), b"\n"], *caught),
            Piece::Flush | Piece::Sync => {
                match model.begin(Work::Flush, 0) {
                    Went::Done => landed = written.len(),
                    Went::Killed(_) => {
                        return Ended::Killed(Some((
                            file,
                            written.get(..landed).unwrap_or_default().to_vec(),
                        )));
                    }
                    Went::Failed(fault, _) => return failed(fault, None),
                }
                if matches!(piece, Piece::Sync) {
                    match model.begin(Work::Sync, 0) {
                        Went::Done => {}
                        Went::Killed(_) => return Ended::Killed(Some((file, written.clone()))),
                        Went::Failed(fault, _) => return failed(fault, None),
                    }
                }
                continue;
            }
        };
        for data in writes {
            match model.begin(Work::Write, data.len()) {
                Went::Done => written.extend_from_slice(data),
                Went::Killed(through) => {
                    written.extend_from_slice(data.get(..through).unwrap_or_default());
                    return Ended::Killed(Some((file, written)));
                }
                Went::Failed(fault, through) => {
                    written.extend_from_slice(data.get(..through).unwrap_or_default());
                    if !caught {
                        return failed(fault, None);
                    }
                    let _ = writeln!(printed, "caught {}", fault.printed(None));
                    break;
                }
            }
        }
    }
    match model.begin(Work::Flush, 0) {
        Went::Done => {}
        Went::Killed(_) => {
            return Ended::Killed(Some((file, written.get(..landed).unwrap_or_default().to_vec())));
        }
        Went::Failed(fault, _) => return failed(fault, None),
    }
    if durable {
        match model.begin(Work::Sync, 0) {
            Went::Done => {}
            Went::Killed(_) => return Ended::Killed(Some((file, written))),
            Went::Failed(fault, _) => return failed(fault, None),
        }
    }
    match model.begin(Work::Rename, 0) {
        Went::Done => {}
        Went::Killed(_) => return Ended::Killed(Some((file, written))),
        Went::Failed(fault, _) => return failed(fault, None),
    }
    if let Some(target) = files.get_mut(file) {
        *target = Some(written);
    }
    if durable {
        match model.begin(Work::Sync, 0) {
            Went::Done => {}
            Went::Killed(_) => return Ended::Killed(None),
            Went::Failed(fault, _) => return failed(fault, None),
        }
    }
    Ended::Printed(format!("closed {at}\n"))
}

/// The sets of rules a program runs with, each failing one or two pieces of the work a run without
/// rules does, or one past it. A kill is left out where a process can't be killed as `SIGKILL`
/// does.
fn rule_sets(plan: &Plan) -> Vec<Vec<Rule>> {
    let (_, work) = modelled(plan, &[]);
    let mut bytes = Bytes::new(&plan.choices);
    (0..FAULTED_RUNS)
        .map(|_| {
            let mut rules: Vec<Rule> = Vec::new();
            for _ in 0..=usize::from(bytes.below(3) == 0) {
                let picked = bytes.below(work.len() + 1);
                let (kind, length, n) = if let Some(&(kind, length)) = work.get(picked) {
                    let n = work.iter().take(picked + 1).filter(|(each, _)| *each == kind).count();
                    (kind, length, n)
                } else {
                    let kind = WORKS.get(bytes.below(WORKS.len())).copied().unwrap_or(Work::Write);
                    (kind, 0, 1 + work.iter().filter(|(each, _)| *each == kind).count())
                };
                let mut fault = FAULTS.get(bytes.below(FAULTS.len())).copied().unwrap_or(Fault::Io);
                if fault == Fault::Kill && !cfg!(unix) {
                    fault = Fault::Io;
                }
                let through = if kind == Work::Write { bytes.below(length + 1) } else { 0 };
                if !rules.iter().any(|rule| rule.work == kind && rule.n == n) {
                    rules.push(Rule { work: kind, n, fault, bytes: through });
                }
            }
            rules
        })
        .collect()
}

/// The files a fresh folder holds beside its targets, as runs before left them: one by a run that's
/// gone, which a run's first write into the folder clears, and one by a run still going, this
/// one, which it never does (#2238).
struct Seeded {
    gone: String,
    live: String,
}

/// What the file a run still going left holds.
const LIVE: &[u8] = b"still being written";

/// A fresh folder under `work`, holding each file's old text, and beside them what two earlier
/// runs left.
fn fresh(work: &Path) -> Result<(PathBuf, Seeded), String> {
    let folder = work.join("folder");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    for (name, old) in FILES {
        let file = folder.join(name);
        std::fs::write(&file, old).map_err(|err| format!("{}: {err}", file.display()))?;
    }
    // A process number no process has: one that's just ended.
    let gone = Command::new("true")
        .spawn()
        .and_then(|mut child| child.wait().map(|_| child.id()))
        .map_err(|err| format!("couldn't run `true`: {err}"))?;
    let seeded = Seeded {
        gone: format!(".f.txt.{gone}.1.fennec-partial"),
        live: format!(".g.txt.{}.1.fennec-partial", std::process::id()),
    };
    for (name, held) in [(&seeded.gone, b"left by a gone run".as_slice()), (&seeded.live, LIVE)] {
        let file = folder.join(name);
        std::fs::write(&file, held).map_err(|err| format!("{}: {err}", file.display()))?;
    }
    Ok((folder, seeded))
}

/// What a run left in `folder`: each file, and the file of a write's own beside one, named
/// `.NAME.PID.N.fennec-partial`. The one a gone run left must be cleared, since every program
/// writes into the folder, and the one a run still going left must be there as it was. Anything
/// else there is a failure.
fn found(folder: &Path, seeded: &Seeded, (printed, killed): (Option<String>, bool)) -> Result<Left, String> {
    let mut left = Left { printed, killed, files: [None, None], partial: None };
    let entries = std::fs::read_dir(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    let mut live = false;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let held = std::fs::read(entry.path()).map_err(|err| format!("{name}: {err}"))?;
        if name == seeded.gone {
            return Err(format!("`{name}`, left by a run that's gone, wasn't cleared"));
        }
        if name == seeded.live {
            if held != LIVE {
                return Err(format!("`{name}`, left by a run still going, was changed"));
            }
            live = true;
            continue;
        }
        if let Some(file) = FILES.iter().position(|(each, _)| *each == name) {
            if let Some(slot) = left.files.get_mut(file) {
                *slot = Some(held);
            }
            continue;
        }
        let beside = FILES.iter().position(|(each, _)| {
            name.starts_with(&format!(".{each}.")) && name.ends_with(".fennec-partial")
        });
        match (beside, &left.partial) {
            (Some(file), None) => left.partial = Some((file, held)),
            _ => return Err(format!("the run left `{name}` too")),
        }
    }
    if !live {
        return Err(format!("`{}`, left by a run still going, was cleared", seeded.live));
    }
    Ok(left)
}

/// Runs `executable` in `folder` with `rules`: what it printed, what it said, and whether a kill
/// ended it.
fn ran(
    executable: &Path,
    work: &Path,
    folder: &Path,
    rules: &[Rule],
) -> Result<(String, String, bool), String> {
    let (out, err) = (work.join("stdout"), work.join("stderr"));
    let file = |path: &Path| std::fs::File::create(path).map_err(|err| format!("{}: {err}", path.display()));
    let mut child = Command::new(executable)
        .current_dir(folder)
        .env("FENNEC_FAULTS", spec(rules))
        .stdin(Stdio::null())
        .stdout(file(&out)?)
        .stderr(file(&err)?)
        .spawn()
        .map_err(|err| format!("couldn't run what LLVM built: {err}"))?;
    let deadline = Instant::now() + STUCK;
    // A kill a rule asked for ends the process by a signal, which the watch counts as a crash: what
    // ended it is judged below, from its status.
    if let Some(failure @ (Failure::Stuck | Failure::TooMuchMemory)) =
        watch(&mut child, STUCK, Some(deadline), MOST_KILOBYTES, || false)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("a run {failure:?}"));
    }
    let status = child.wait().map_err(|err| format!("couldn't wait for the program: {err}"))?;
    let printed = String::from_utf8_lossy(&std::fs::read(&out).unwrap_or_default()).into_owned();
    let said = String::from_utf8_lossy(&std::fs::read(&err).unwrap_or_default()).into_owned();
    #[cfg(unix)]
    let killed = {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal() == Some(9)
    };
    #[cfg(not(unix))]
    let killed = false;
    if !killed && (status.code() != Some(0) || !said.is_empty()) {
        return Err(format!("it ended with {status}, printing:\n{printed}\nand saying:\n{said}"));
    }
    Ok((printed, said, killed))
}

/// Where `fennec` beside a runtime built with the `faults` feature is.
fn faulted_fennec() -> PathBuf {
    std::env::var_os("FENNEC_FAULTED").map_or_else(crate::llvm::fennec, PathBuf::from)
}

/// A program whose first write a rule fails, built both ways, must say so: a runtime that ships
/// reads no rules, and every run would pass without testing anything.
fn canary() -> Result<(), String> {
    static CHECKED: OnceLock<Result<(), String>> = OnceLock::new();
    CHECKED
        .get_or_init(|| {
            let source = "def main(stdout: Stdout, fs: Fs):\n    fs.write_text(\"f.txt\", \"new\") except why:\n        \
                          stdout.print(\"failed\", why)\n";
            let work = crate::llvm::work().join("fault-writes-canary");
            let rule = [Rule { work: Work::Write, n: 1, fault: Fault::NoSpace, bytes: 0 }];
            for release in [false, true] {
                let executable = crate::llvm::build_with(&faulted_fennec(), source, &work, release)?
                    .ok_or("LLVM doesn't take the canary")?;
                let (folder, _) = fresh(&work)?;
                let (printed, _, _) = ran(&executable, &work, &folder, &rule)?;
                if printed != format!("failed {}\n", Fault::NoSpace.printed(None)) {
                    return Err(format!(
                        "`{}` builds programs whose runtime doesn't read FENNEC_FAULTS: point FENNEC_FAULTED at a \
                         `fennec` beside a runtime built with the `faults` feature (it printed {printed:?})",
                        faulted_fennec().display()
                    ));
                }
            }
            Ok(())
        })
        .clone()
}

pub(crate) fn fault_writes(input: &[u8]) -> Result<(), String> {
    canary()?;
    let plan = plan(input);
    let source = source(&plan);
    let work = crate::llvm::work().join("fault-writes");
    let mut runs = vec![Vec::new()];
    runs.extend(rule_sets(&plan));
    for release in [false, true] {
        let Some(executable) = crate::llvm::build_with(&faulted_fennec(), &source, &work, release)? else {
            return Err(format!("LLVM doesn't take a program this target writes:\n{source}"));
        };
        for rules in &runs {
            let (wanted, _) = modelled(&plan, rules);
            let (folder, seeded) = fresh(&work)?;
            let how = if release { "--release" } else { "a dev build" };
            let (printed, _, killed) = ran(&executable, &work, &folder, rules)
                .map_err(|err| format!("{how}, {}: {err}\n{source}", spec(rules)))?;
            let left = found(&folder, &seeded, ((!killed).then_some(printed), killed))
                .map_err(|err| format!("{how}, {}: {err}\n{source}", spec(rules)))?;
            if left != wanted {
                return Err(format!(
                    "{how}, with FENNEC_FAULTS={}, left\n{}\nwhere the model left\n{}\n{source}",
                    spec(rules),
                    shown(&left),
                    shown(&wanted)
                ));
            }
        }
    }
    Ok(())
}

/// What a run left, to read.
fn shown(left: &Left) -> String {
    let text = |bytes: &Option<Vec<u8>>| {
        bytes.as_ref().map_or("(none)".to_owned(), |bytes| format!("{:?}", String::from_utf8_lossy(bytes)))
    };
    let mut out = match &left.printed {
        Some(printed) => format!("printed:\n{printed}"),
        None => "killed\n".to_owned(),
    };
    for ((name, _), held) in FILES.iter().zip(&left.files) {
        let _ = writeln!(out, "{name}: {}", text(held));
    }
    if let Some((file, held)) = &left.partial {
        let _ = writeln!(out, "beside {}: {:?}", file_name(*file), String::from_utf8_lossy(held));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Act, Fault, Piece, Plan, Rule, Work, modelled, plan, rule_sets, source};

    /// Programs of every kind of act and piece check.
    #[test]
    fn every_way_writes_a_program_that_checks() {
        for seed in 0_u8..64 {
            let input: Vec<u8> = (0_u8..48).map(|at| at.wrapping_mul(seed).wrapping_add(seed)).collect();
            let plan = plan(&input);
            let source = source(&plan);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            assert_eq!(rule_sets(&plan).len(), super::FAULTED_RUNS);
        }
    }

    /// The model keeps what landed of a write a block caught, as the runtime lane's test does:
    /// "one\n", then "two\n" cut to "two", then "three\n".
    #[test]
    fn a_caught_write_keeps_what_landed() {
        let line = |text: &str| Piece::Line { text: text.to_owned(), caught: true };
        let plan = Plan {
            acts: vec![Act::Block {
                file: 0,
                durable: true,
                pieces: vec![line("one"), line("two"), line("three")],
            }],
            choices: Vec::new(),
        };
        let rule = [Rule { work: Work::Write, n: 4, fault: Fault::NoSpace, bytes: 0 }];
        let (left, work) = modelled(&plan, &rule);
        assert_eq!(left.files[0].as_deref(), Some(b"one\ntwothree\n".as_slice()));
        assert!(left.printed.is_some_and(|printed| printed.starts_with("caught the disk is full")));
        assert_eq!(work.iter().filter(|(kind, _)| *kind == Work::Write).count(), 6);
    }
}
