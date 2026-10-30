//! The `fennec` command.
#![expect(clippy::print_stdout, reason = "the command line prints its results")]
#![expect(clippy::print_stderr, reason = "the command line reports errors")]

mod repl;
mod run;
mod test;

use std::process::ExitCode;

const USAGE: &str = "usage:
  fennec run <file.fen>    run a program's main
  fennec run --jit <file>  run it as native code (still in progress)
  fennec test <file.fen>   run a program's tests
  fennec repl              try Fennec interactively
  fennec --version         show the version";

/// The interpreter recurses for each nested call, so it gets a generous stack.
const STACK_BYTES: usize = 512 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["run", path] => on_big_stack(run::run, path),
        ["run", "--jit", path] => on_big_stack(run::run_jit, path),
        ["test", path] => on_big_stack(test::test, path),
        ["repl"] => on_big_stack(|_| repl::repl(), ""),
        ["dev", "tokens", path] => dev(fennec_syntax::dump::tokens, path),
        ["dev", "tree", path] => dev(fennec_syntax::dump::tree, path),
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

fn on_big_stack(command: fn(&str) -> ExitCode, path: &str) -> ExitCode {
    let path = path.to_owned();
    let worker = std::thread::Builder::new().stack_size(STACK_BYTES).spawn(move || command(&path));
    if let Ok(Ok(code)) = worker.map(std::thread::JoinHandle::join) {
        code
    } else {
        eprintln!("error: the interpreter stopped unexpectedly");
        ExitCode::FAILURE
    }
}

/// `fennec dev tokens <file>` and `fennec dev tree <file>`: hidden commands that print a dump
/// of the front end's work, for comparing with the self-hosted front end in `selfhost/`.
fn dev(dump: fn(&str) -> String, path: &str) -> ExitCode {
    match std::fs::read_to_string(path) {
        Ok(source) => {
            print!("{}", dump(&source));
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: couldn't read {path}: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Reads and parses a file, printing the problem if there is one.
fn load(path: &str) -> Option<(String, fennec_syntax::Module)> {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: couldn't read {path}: {err}");
            return None;
        }
    };
    match fennec_syntax::parse(&source) {
        Ok(module) => Some((source, module)),
        Err(diagnostic) => {
            eprint!("{}", diagnostic.render(path, &source));
            None
        }
    }
}
