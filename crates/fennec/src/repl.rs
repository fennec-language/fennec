//! `fennec repl`: type Fennec and see the result, like Python's `>>>`. A line ending in `:`
//! or with an open bracket continues until a blank line.

use std::io::{BufRead, Write};
use std::process::ExitCode;

pub(crate) fn repl() -> ExitCode {
    println!("🦊 fennec {}: type an expression, or `exit` to leave", env!("CARGO_PKG_VERSION"));
    let mut out = std::io::stdout();
    let mut interp = match fennec_interp::Interp::new(&mut out) {
        Ok(interp) => interp,
        Err(diagnostic) => {
            eprintln!("error: {}", diagnostic.message);
            return ExitCode::FAILURE;
        }
    };
    interp.start_repl();
    let mut lines = std::io::stdin().lock().lines();
    while let Some(chunk) = read_chunk(&mut lines) {
        let trimmed = chunk.trim();
        if trimmed == "exit" || trimmed == "quit" {
            break;
        }
        if trimmed.is_empty() {
            continue;
        }
        let result = fennec_syntax::parse_repl(&chunk).and_then(|module| interp.eval_repl(&module));
        match result {
            Ok(Some(echo)) => println!("{echo}"),
            Ok(None) => {}
            Err(diagnostic) => eprint!("{}", diagnostic.render("<repl>", &chunk)),
        }
    }
    println!();
    ExitCode::SUCCESS
}

/// Reads one entry: a line, or a block that continues until a blank line.
fn read_chunk(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> Option<String> {
    prompt(">>> ");
    let mut chunk = lines.next()?.ok()?;
    if needs_more(&chunk) {
        loop {
            prompt("... ");
            let Some(Ok(line)) = lines.next() else { break };
            if line.trim().is_empty() && depth(&chunk) <= 0 {
                break;
            }
            chunk.push('\n');
            chunk.push_str(&line);
        }
    }
    chunk.push('\n');
    Some(chunk)
}

fn prompt(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

fn needs_more(line: &str) -> bool {
    line.trim_end().ends_with(':') || depth(line) > 0
}

/// How many brackets are still open.
fn depth(text: &str) -> i32 {
    text.chars().fold(0, |depth, c| match c {
        '(' | '[' | '{' => depth + 1,
        ')' | ']' | '}' => depth - 1,
        _ => depth,
    })
}
