//! `fennec dev fallbacks [path]`: every place a program's compiled code isn't native, with
//! why. Code inside loops comes first, deepest first, since that's where speed goes.

use std::process::ExitCode;

pub(crate) fn fallbacks(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let mut notes = match fennec_jit::fallbacks(&program.module) {
        Ok(notes) => notes,
        Err(diagnostic) => {
            eprint!("{}", program.sources.render(&diagnostic));
            return ExitCode::FAILURE;
        }
    };
    if notes.is_empty() {
        println!("everything compiles to native code");
        return ExitCode::SUCCESS;
    }
    notes.sort_by_key(|note| std::cmp::Reverse(note.loops));
    for note in &notes {
        let place = program.sources.locate(note.span);
        let (path, line, column, text) = place.unwrap_or(("?", 0, 0, ""));
        let depth = match note.loops {
            0 => String::new(),
            1 => " (in a loop)".to_owned(),
            n => format!(" (in {n} loops)"),
        };
        println!("{path}:{line}:{column} in `{}`{depth}: {}", note.function, note.what);
        println!("    {}", text.trim());
    }
    println!("{} places aren't native", notes.len());
    ExitCode::SUCCESS
}
