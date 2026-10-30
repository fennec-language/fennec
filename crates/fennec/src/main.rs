//! The `fennec` command.
#![expect(clippy::print_stdout, reason = "the command line prints its results")]
#![expect(clippy::print_stderr, reason = "the command line reports errors")]

mod repl;
mod run;
mod test;

use std::process::ExitCode;

const USAGE: &str = "usage:
  fennec run [path]         run a kit's main, or a single .fen file's
  fennec run --jit [path]   run it as native code (still in progress)
  fennec test [path]        run a kit's tests, or a single file's
  fennec new <name>         start a kit in a new folder
  fennec repl               try Fennec interactively
  fennec --version          show the version

  path is a .fen file, or a kit's folder; it defaults to the current folder";

/// The interpreter recurses for each nested call, so it gets a generous stack.
const STACK_BYTES: usize = 512 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["run"] => on_big_stack(run::run, "."),
        ["run", "--jit"] => on_big_stack(run::run_jit, "."),
        ["run", "--jit", path] => on_big_stack(run::run_jit, path),
        ["run", path] => on_big_stack(run::run, path),
        ["test"] => on_big_stack(test::test, "."),
        ["test", path] => on_big_stack(test::test, path),
        ["new", path] => new(path),
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

/// `fennec new`: makes a starter kit.
fn new(path: &str) -> ExitCode {
    match fennec_kit::create(std::path::Path::new(path)) {
        Ok(name) => {
            println!("🦊 made the kit `{name}` in {path}");
            println!("   run it with: cd {path} && fennec run");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Loads a program from a file or a kit's folder, printing the problem if there is one.
fn load(path: &str, entry: fennec_kit::Entry) -> Option<fennec_kit::Program> {
    match fennec_kit::load(std::path::Path::new(path), path, entry) {
        Ok(program) => Some(program),
        Err(rendered) => {
            eprint!("{rendered}");
            None
        }
    }
}
