//! The `killed-writes` target: a program puts a new file in place of an old one and is killed with
//! `SIGKILL` partway. The file must then be the old one or the new one, byte for byte, never torn:
//! what's written goes into a file of its own beside the path, which takes the path's place at
//! once when the write ends. A run that isn't killed must leave the new file and print `done`.
//!
//! The input chooses how the program writes: through `with fs.create(...)` a piece at a time, with
//! `durable=False` too, or whole, through `fs.write_text` or `fs.write_bytes`. It also chooses the
//! piece, how many times it's written, and when each of six runs is killed, as a share of how long
//! a whole run took. A run killed after the program finished leaves the new file, which is fine.
//!
//! What a killed run leaves beside the file, as the file of its own it was writing, isn't held to
//! anything here: nothing's left to clear it. A full disk or an I/O error partway needs a hook in
//! the runtime to make one, and isn't covered.
//!
//! The program is built once, with `fennec build --llvm`, and needs what `llvm` needs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::supervise::{MOST_KILOBYTES, STUCK, watch};

/// How long a whole run may take on the clock before it counts as stuck.
const LONGEST: Duration = Duration::from_secs(30);

/// What the file holds before each run, which no piece can make, since none holds `O`, `L` or
/// `D`.
const OLD: &str = "OLD CONTENT\n";
/// How many times `OLD` is in the old file.
const OLD_TIMES: usize = 400;

/// The characters a piece is made of: letters and digits, a space, a line's end, a letter past
/// ASCII and an emoji.
const CHARACTERS: [&str; 16] =
    ["a", "b", "c", "x", "y", "z", "0", "1", "7", "9", " ", "\n", "-", "X", "\u{e9}", "\u{1f98a}"];

/// When each killed run is killed, in thousandths of a whole run, if the input doesn't say.
const KILLED_AT: [u32; 6] = [0, 100, 300, 500, 700, 950];

/// How the program writes the new file.
#[derive(Clone, Copy, Debug)]
enum Way {
    /// `with fs.create("f.txt")? as file:`, writing the piece a time at a time.
    Create,
    /// The same with `durable=False`.
    Undurable,
    /// `fs.write_text("f.txt", piece * times)?`.
    Text,
    /// `fs.write_bytes("f.txt", (piece * times).encode())?`.
    Bytes,
}

/// What the input chooses.
struct Plan {
    way: Way,
    piece: String,
    times: usize,
    /// When each killed run is killed, in thousandths of a whole run.
    killed_at: Vec<u32>,
}

impl Plan {
    /// What the new file holds.
    fn new_text(&self) -> String {
        self.piece.repeat(self.times)
    }
}

fn plan(input: &[u8]) -> Plan {
    let byte = |at: usize| input.get(at).copied().unwrap_or(0);
    let way = match byte(0) % 4 {
        0 => Way::Create,
        1 => Way::Undurable,
        2 => Way::Text,
        _ => Way::Bytes,
    };
    let length = 1 + usize::from(byte(1) % 48);
    let times = 1024 + usize::from(u16::from_le_bytes([byte(2), byte(3)]) % 32768);
    let piece = (0..length)
        .map(|at| CHARACTERS.get(usize::from(byte(4 + at)) % CHARACTERS.len()).copied().unwrap_or("a"))
        .collect();
    let rest = input.get(4 + length..).unwrap_or_default();
    let killed_at = if rest.len() >= KILLED_AT.len() {
        rest.iter().take(KILLED_AT.len()).map(|byte| u32::from(*byte) * 1000 / 255).collect()
    } else {
        KILLED_AT.to_vec()
    };
    Plan { way, piece, times, killed_at }
}

/// `text` as a Fennec string literal.
fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn source(plan: &Plan) -> String {
    let (piece, times) = (literal(&plan.piece), plan.times);
    let mut out = format!("def main(stdout: Stdout, fs: Fs):\n    piece = {piece}\n");
    let _ = match plan.way {
        Way::Create | Way::Undurable => {
            let durable = if matches!(plan.way, Way::Undurable) { ", durable=False" } else { "" };
            write!(
                out,
                "    with fs.create(\"f.txt\"{durable})? as file:\n        for _ in range({times}):\n            \
                 file.write_text(piece)?\n"
            )
        }
        Way::Text => writeln!(out, "    fs.write_text(\"f.txt\", piece * {times})?"),
        Way::Bytes => writeln!(out, "    fs.write_bytes(\"f.txt\", (piece * {times}).encode())?"),
    };
    out.push_str("    stdout.print(\"done\")\n    return\n");
    out
}

/// The program the input makes, for `fennec-fuzz show killed-writes`.
pub(crate) fn written(input: &[u8]) -> String {
    source(&plan(input))
}

/// A fresh folder under `work` holding the old file.
fn fresh(work: &Path) -> Result<PathBuf, String> {
    let folder = work.join("folder");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    let file = folder.join("f.txt");
    std::fs::write(&file, OLD.repeat(OLD_TIMES)).map_err(|err| format!("{}: {err}", file.display()))?;
    Ok(folder)
}

/// Runs `executable` in `folder`, killing it with `SIGKILL` after `killed_after` when that's
/// given, and otherwise stopping it only if it gets stuck. What it printed, what it said, and its
/// exit code, which a killed run has none of.
fn ran(
    executable: &Path,
    work: &Path,
    folder: &Path,
    killed_after: Option<Duration>,
) -> Result<(Vec<u8>, String, Option<i32>), String> {
    let (out, err) = (work.join("stdout"), work.join("stderr"));
    let file = |path: &Path| std::fs::File::create(path).map_err(|err| format!("{}: {err}", path.display()));
    let mut child = Command::new(executable)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(file(&out)?)
        .stderr(file(&err)?)
        .spawn()
        .map_err(|err| format!("couldn't run what LLVM built: {err}"))?;
    if let Some(after) = killed_after {
        std::thread::sleep(after);
        // `SIGKILL`, which no program can catch or clean up after.
        let _ = child.kill();
    } else {
        let deadline = Instant::now() + LONGEST;
        if let Some(failure) = watch(&mut child, STUCK, Some(deadline), MOST_KILOBYTES, || false) {
            let _ = child.wait();
            return Err(format!("a whole run {failure:?}"));
        }
    }
    let status = child.wait().map_err(|err| format!("couldn't wait for the program: {err}"))?;
    let printed = std::fs::read(&out).unwrap_or_default();
    let said = String::from_utf8_lossy(&std::fs::read(&err).unwrap_or_default()).into_owned();
    Ok((printed, said, status.code()))
}

/// Where `held` first differs from `wanted`, and their lengths, to say how a file is torn.
fn differs(held: &[u8], wanted: &[u8]) -> String {
    let at = held.iter().zip(wanted).take_while(|(one, other)| one == other).count();
    format!("first differs at byte {at}, holding {} bytes against {}", held.len(), wanted.len())
}

pub(crate) fn killed_writes(input: &[u8]) -> Result<(), String> {
    let plan = plan(input);
    let source = source(&plan);
    let work = crate::llvm::work().join("killed-writes");
    let Some(executable) = crate::llvm::build(&source, &work, false)? else {
        return Err(format!("LLVM doesn't take a program this target writes:\n{source}"));
    };
    let (old, new) = (OLD.repeat(OLD_TIMES).into_bytes(), plan.new_text().into_bytes());
    let folder = fresh(&work)?;
    let started = Instant::now();
    let (printed, said, code) = ran(&executable, &work, &folder, None)?;
    let took = started.elapsed();
    if code != Some(0) || printed != b"done\n" || !said.is_empty() {
        return Err(format!(
            "a whole run ended with {code:?}, printing {printed:?} and saying:\n{said}\n{source}"
        ));
    }
    let held =
        std::fs::read(folder.join("f.txt")).map_err(|err| format!("after a whole run, f.txt: {err}"))?;
    if held != new {
        return Err(format!("a whole run didn't leave the new file: {}\n{source}", differs(&held, &new)));
    }
    for &thousandths in &plan.killed_at {
        let folder = fresh(&work)?;
        let after = took * thousandths / 1000;
        ran(&executable, &work, &folder, Some(after))?;
        let held = std::fs::read(folder.join("f.txt"))
            .map_err(|err| format!("killed after {after:?} of {took:?}, f.txt is gone: {err}\n{source}"))?;
        if held != old && held != new {
            return Err(format!(
                "killed after {after:?} of {took:?}, f.txt is torn: against the old file it {}, and against the \
                 new it {}\n{source}",
                differs(&held, &old),
                differs(&held, &new)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{plan, source};

    /// Each way writes a program that checks.
    #[test]
    fn every_way_writes_a_program_that_checks() {
        for way in 0..4_u8 {
            let input = [way, 5, 1, 0, 2, 11, 14, 15, 3, 10];
            let source = source(&plan(&input));
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
        }
    }
}
