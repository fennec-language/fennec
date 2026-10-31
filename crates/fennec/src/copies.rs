//! `fennec dev copies [path]`: every place a program may copy a large value or allocate, with
//! why. Code that runs in loops comes first, since that's where it costs the most.
//!
//! `fennec dev copies --run [path]` also runs the program and says what it actually did, which
//! is how to show that a hot path copies and allocates nothing. What it prints is left out.

use std::process::ExitCode;

/// What the report says, then what a run of the program did.
pub(crate) fn copies_in_a_run(path: &str) -> ExitCode {
    let code = copies(path);
    if code != ExitCode::SUCCESS {
        return code;
    }
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    match fennec_jit::run_with_report(&program.module, &mut std::io::sink()) {
        Ok(report) => {
            println!("{}", ran(&report.counts));
            if !report.interpreted.is_empty() || report.handed_over > 0 {
                println!("only native code is counted, and part of this program ran in the interpreter");
            }
            ExitCode::SUCCESS
        }
        Err(fennec_jit::JitError::Failed(diagnostic) | fennec_jit::JitError::Unsupported(diagnostic)) => {
            eprint!("{}", program.sources.render(&diagnostic));
            ExitCode::FAILURE
        }
    }
}

/// What a run did, in a line.
fn ran(counts: &fennec_interp::embed::Counts) -> String {
    let copies = match counts.copies {
        0 => "no copies".to_owned(),
        1 => format!("1 copy of {}", bytes(counts.copied_bytes)),
        count => format!(
            "{count} copies of {}, the largest {}",
            bytes(counts.copied_bytes),
            bytes(counts.largest_copy)
        ),
    };
    let allocations = match counts.allocations {
        1 => "1 allocation".to_owned(),
        count => format!("{count} allocations"),
    };
    format!("in a run: {copies}, {allocations} of {}", bytes(counts.allocated_bytes))
}

/// A number of bytes, in the unit that reads best.
#[expect(clippy::cast_precision_loss, reason = "a size in a report doesn't need every digit")]
fn bytes(count: u64) -> String {
    match count {
        0..1_024 => format!("{count} B"),
        1_024..1_048_576 => format!("{:.1} KB", count as f64 / 1_024.0),
        _ => format!("{:.1} MB", count as f64 / 1_048_576.0),
    }
}

pub(crate) fn copies(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if let Err(diagnostics) = fennec_check::check(&program.module) {
        eprint!("{}", program.sources.render_all(&diagnostics));
        return ExitCode::FAILURE;
    }
    // Only what runs matters. It checked whole, so what's left does too.
    fennec_build::keep_what_main_reaches(&mut program.module);
    let checked = match fennec_check::check_and_resolve(&mut program.module) {
        Ok(checked) => checked,
        Err(diagnostics) => {
            eprint!("{}", program.sources.render_all(&diagnostics));
            return ExitCode::FAILURE;
        }
    };
    let lowered = fennec_ir::lower(&program.module, &checked);
    let findings = fennec_ir::costs(&lowered);
    for finding in &findings {
        let (file, line, column, text) = program.sources.locate(finding.span).unwrap_or(("?", 0, 0, ""));
        let hot = match (finding.loops, &finding.called_from_loop) {
            (0, None) => String::new(),
            (0, Some(caller)) => format!(" (called from a loop in `{caller}`)"),
            (1, _) => " (in a loop)".to_owned(),
            (loops, _) => format!(" (in {loops} loops)"),
        };
        println!("{file}:{line}:{column} in `{}`{hot}: {}", finding.function, finding.message());
        println!("    {}", text.trim());
    }
    for skipped in &lowered.not_lowered {
        println!("not looked at: `{}`, since {}", skipped.function, skipped.why);
    }
    let hot = findings.iter().filter(|finding| finding.is_hot()).count();
    println!("{} places may copy or allocate, {hot} of them in loops", findings.len());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{bytes, ran};
    use fennec_interp::embed::Counts;

    #[test]
    fn sizes_read_in_the_unit_that_fits() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1_023), "1023 B");
        assert_eq!(bytes(1_024), "1.0 KB");
        assert_eq!(bytes(1_048_575), "1024.0 KB");
        assert_eq!(bytes(1_048_576), "1.0 MB");
    }

    #[test]
    fn a_run_says_what_it_did() {
        assert_eq!(ran(&Counts::default()), "in a run: no copies, 0 allocations of 0 B");
        let counts = Counts {
            allocations: 2,
            allocated_bytes: 80,
            copies: 3,
            copied_bytes: 2_048,
            largest_copy: 1_024,
        };
        assert_eq!(ran(&counts), "in a run: 3 copies of 2.0 KB, the largest 1.0 KB, 2 allocations of 80 B");
        let counts =
            Counts { allocations: 1, allocated_bytes: 8, copies: 1, copied_bytes: 8, largest_copy: 8 };
        assert_eq!(ran(&counts), "in a run: 1 copy of 8 B, 1 allocation of 8 B");
    }
}
