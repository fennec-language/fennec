//! The `fennec` command.
#![expect(clippy::print_stdout, reason = "the command line prints its results")]
#![expect(clippy::print_stderr, reason = "the command line reports errors")]

mod aot;
mod changes;
mod daemon;
mod fallbacks;
mod fix;
mod fmt;
mod lsp;
mod repl;
mod run;
mod test;

use std::process::ExitCode;

const USAGE: &str = "usage:
  fennec run [path] [args]  run a kit's main, or a single .fen file's, passing it args
  fennec run --interp [path]  run it with the interpreter instead of compiling it
  fennec build [path] [-o out]  compile it to a standalone executable
  fennec test [path]        run a kit's tests, or a single file's, skipping unchanged ones that passed
  fennec test --all [path]  run every test
  fennec test --verbose [path]  list the tests that pass too, not only failures and the summary
  fennec new <name>         start a kit in a new folder
  fennec fmt [path...]      format files in place: a kit, a folder or a .fen file
  fennec fmt --check        list the files that aren't formatted, and fail if any aren't
  fennec fix [path]         make the fixes the type checker suggests, in a kit or a .fen file
  fennec lsp                the language server, for editors, over standard input and output
  fennec daemon <action>    the build daemon: `status` or `stop`; commands start it
  fennec repl               try Fennec interactively
  fennec --version          show the version

  path is a .fen file, or a kit's folder; it defaults to the current folder";

/// The interpreter recurses for each nested call, so it gets a generous stack.
pub(crate) const STACK_BYTES: usize = 512 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["run"] => run(run::Engine::Native, ".", &[]),
        ["run", "--interp"] => run(run::Engine::Interpreter, ".", &[]),
        ["run", "--interp", path, arguments @ ..] => run(run::Engine::Interpreter, path, arguments),
        ["run", "--jit"] => run(run::Engine::NativeNoted, ".", &[]),
        ["run", "--jit", path, arguments @ ..] => run(run::Engine::NativeNoted, path, arguments),
        ["run", path, arguments @ ..] => run(run::Engine::Native, path, arguments),
        ["build"] => aot::build(".", None),
        ["build", "-o", output] => aot::build(".", Some(output)),
        ["build", path] => aot::build(path, None),
        ["build", path, "-o", output] => aot::build(path, Some(output)),
        ["test", rest @ ..] => test(rest),
        ["new", path] => new(path),
        ["repl"] => on_big_stack(|_| repl::repl(), ""),
        ["fmt", rest @ ..] => fmt::fmt(rest),
        ["fix"] => fix::fix("."),
        ["fix", path] => fix::fix(path),
        ["dev", "startup", path] => {
            let path = (*path).to_owned();
            big_stack(move || run::startup(&path))
        }
        ["dev", "tokens", path] => dev(fennec_syntax::dump::tokens, path),
        ["dev", "tree", path] => dev(fennec_syntax::dump::tree, path),
        ["dev", "check", path] => dev(check_dump, path),
        ["daemon", action] => daemon::command(action),
        ["lsp"] => lsp::lsp(),
        ["dev", "fallbacks"] => on_big_stack(fallbacks::fallbacks, "."),
        ["dev", "fallbacks", path] => on_big_stack(fallbacks::fallbacks, path),
        ["dev", "changes"] => changes::changes("."),
        ["dev", "changes", path] => changes::changes(path),
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
    big_stack(move || command(&path))
}

/// `fennec test [--all] [--verbose] [path]`, on a big stack for the interpreter.
fn test(args: &[&str]) -> ExitCode {
    let Some((path, options)) = test::Options::of(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    big_stack(move || test::test(&path, options))
}

/// `fennec run [--interp] [path] [args]`: runs a program, handing it the arguments after its path.
fn run(engine: run::Engine, path: &str, arguments: &[&str]) -> ExitCode {
    let path = path.to_owned();
    let arguments: Vec<String> = arguments.iter().map(|arg| (*arg).to_owned()).collect();
    big_stack(move || run::run(&path, engine, &arguments))
}

fn big_stack(command: impl FnOnce() -> ExitCode + Send + 'static) -> ExitCode {
    let worker = std::thread::Builder::new().stack_size(STACK_BYTES).spawn(command);
    if let Ok(Ok(code)) = worker.map(std::thread::JoinHandle::join) {
        code
    } else {
        eprintln!("error: the interpreter stopped unexpectedly");
        ExitCode::FAILURE
    }
}

/// `fennec dev tokens|tree|check <file>`: hidden commands that print a dump of the front end's
/// work, for comparing with the self-hosted front end in `selfhost/`.
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

/// What the type checker finds in a file, in the dumps' text: every diagnostic, or the
/// parse error that stopped it first. Nothing when the file checks.
fn check_dump(source: &str) -> String {
    match fennec_syntax::parse(source) {
        Ok(module) => match fennec_check::check(&module) {
            Ok(_) => String::new(),
            Err(diagnostics) => diagnostics.iter().map(fennec_syntax::dump::error).collect(),
        },
        Err(diagnostic) => fennec_syntax::dump::error(&diagnostic),
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
