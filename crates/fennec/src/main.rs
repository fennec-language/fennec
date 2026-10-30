//! The `fennec` command.
#![expect(clippy::print_stdout, reason = "the command line prints its results")]
#![expect(clippy::print_stderr, reason = "the command line reports errors")]

use std::io::{BufWriter, Write};
use std::process::ExitCode;

const USAGE: &str = "usage:
  fennec run <file.fen>   run a program
  fennec --version        show the version";

/// The interpreter recurses for each nested call, so it gets a generous stack.
const STACK_BYTES: usize = 512 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["run", path] => run_on_big_stack(path),
        ["--version" | "-V"] => {
            println!("🦊 fennec {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_on_big_stack(path: &str) -> ExitCode {
    let path = path.to_owned();
    let worker = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || run(&path));
    if let Ok(Ok(code)) = worker.map(std::thread::JoinHandle::join) {
        code
    } else {
        eprintln!("error: the interpreter stopped unexpectedly");
        ExitCode::FAILURE
    }
}

fn run(path: &str) -> ExitCode {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: couldn't read {path}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let module = match fennec_syntax::parse(&source) {
        Ok(module) => module,
        Err(diagnostic) => {
            eprint!("{}", diagnostic.render(path, &source));
            return ExitCode::FAILURE;
        }
    };
    let mut out = BufWriter::new(std::io::stdout().lock());
    let result = fennec_interp::run(&module, &mut out);
    let flushed = out.flush();
    match (result, flushed) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(diagnostic), _) => {
            eprint!("{}", diagnostic.render(path, &source));
            ExitCode::FAILURE
        }
        (Ok(()), Err(err)) => {
            eprintln!("error: couldn't write output: {err}");
            ExitCode::FAILURE
        }
    }
}
