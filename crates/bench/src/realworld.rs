//! `fennec-bench realworld`: the kernels in `bench/realworld`, which use the standard library as
//! a real program does, built through LLVM beside twins that use the crates a Rust program
//! would. Each side runs in turn and its best run counts, as in `llvm_suite`.
//!
//! These aren't in the locked suite, whose twins are one file each with nothing to link. Nothing
//! here fails for being slow: only a kernel that prints something other than its twin does. It
//! prints one Markdown section, and with `--markdown PATH` writes it there too for the suite's
//! report.

use std::fmt::Write as _;
use std::path::Path;

use crate::parallel::{Side, built, shown, twins};

const KERNELS: [&str; 5] = ["game_math", "game_server", "graph", "json_typed", "pack_typed"];
const OUT_DIR: &str = "target/bench/realworld";

/// The section for the report: each kernel's name, with Fennec's side and its twin's.
fn section(kernels: &[(&str, [Side; 2])]) -> String {
    let (cpu, cores) = (crate::machine::cpu(), crate::machine::cores());
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let mut text = format!("## Real programs, on {os} {arch}, {cpu} with {cores} cores\n\n");
    text += "| kernel | Fennec | Rust | Fennec's speed, % of Rust's |\n|---|---:|---:|---:|\n";
    for (name, [ours, rust]) in kernels {
        let (mine, theirs) = (ours.best().0, rust.best().0);
        let percent = if mine > 0.0 { theirs / mine * 100.0 } else { 0.0 };
        let _ = writeln!(text, "| `{name}` | {} | {} | {percent:.0} |", shown(ours), shown(rust));
    }
    text += "\nBest of each side's runs, by wall clock, taking turns. Each uses its language's own \
             libraries: `std.json` against serde and serde_json, and `std.pack` against serde and bincode.\n";
    text
}

/// Whether a kernel printed what its twin did.
fn agree(name: &str, printed: &str, want: &str) -> Result<(), String> {
    if printed == want {
        return Ok(());
    }
    Err(format!("`{name}` printed `{}`, and its twin `{}`", printed.trim(), want.trim()))
}

/// Runs the section. It fails only for a kernel that doesn't build or prints the wrong thing.
pub(crate) fn run(fennec: &Path, dir: &Path, runs: usize, markdown: Option<&Path>) -> Result<(), String> {
    let out = Path::new(OUT_DIR);
    std::fs::create_dir_all(out).map_err(|err| format!("couldn't make {}: {err}", out.display()))?;
    let twin = twins(dir, out)?;
    let mut kernels = Vec::new();
    for name in KERNELS {
        let ours = built(fennec, &dir.join(format!("{name}.fen")), out.join(name), false)?;
        kernels.push((name, [Side::new(ours, None), Side::new(twin.join(name), None)]));
    }
    for _ in 0..runs {
        for (name, [ours, rust]) in &mut kernels {
            agree(name, &ours.run()?, &rust.run()?)?;
        }
    }
    let text = section(&kernels);
    println!("{text}");
    if let Some(path) = markdown {
        std::fs::write(path, &text).map_err(|err| format!("couldn't write {}: {err}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Side, agree, section};
    use std::time::Duration;

    fn side(millis: u64) -> Side {
        let mut side = Side::new("program".into(), None);
        side.times = vec![Duration::from_millis(millis); 3];
        side
    }

    #[test]
    fn the_section_shows_each_kernel_against_its_twin() {
        let text = section(&[("json_typed", [side(200), side(50)])]);
        assert!(text.contains("| `json_typed` | 200 ms | 50 ms | 25 |"), "{text}");
    }

    #[test]
    fn a_kernel_with_no_time_to_compare_shows_no_speed() {
        let text = section(&[("json_typed", [side(0), side(50)])]);
        assert!(text.contains("| `json_typed` | 0 ms | 50 ms | 0 |"), "{text}");
    }

    #[test]
    fn a_kernel_has_to_print_what_its_twin_does() {
        assert_eq!(agree("json_typed", "1 2\n", "1 2\n"), Ok(()));
        let wrong = agree("json_typed", "1 2\n", "1 3\n");
        assert_eq!(wrong, Err("`json_typed` printed `1 2`, and its twin `1 3`".to_owned()));
    }
}
