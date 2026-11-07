//! The `fennec` command.
#![expect(clippy::print_stdout, reason = "the command line prints its results")]
#![expect(clippy::print_stderr, reason = "the command line reports errors")]

/// What `fennec` allocates through, programs it runs included.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod aot;
mod bench;
mod changes;
mod copies;
mod copy_rule;
mod daemon;
mod fallbacks;
mod fix;
mod fmt;
mod image;
mod lint;
mod llvm_tests;
mod lsp;
mod notices;
mod report;
mod run;
mod test;
mod test_inputs;
mod trace;
mod verdict;

/// A path in the temporary folder for this command's own scratch work, named so that nothing
/// else can have it: the process's id and when it asked, as the temporary folder is shared by
/// every process, and one killed before it tidied up can leave a tool it started still writing to
/// a folder named by an id the system has given out again.
pub(crate) fn own_temp(what: &str) -> std::path::PathBuf {
    static ASKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let count = ASKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("fennec-{what}-{}-{at:x}-{count}", std::process::id()))
}

use std::process::ExitCode;

const USAGE: &str = "usage:
  fennec run [path] [args]  run the tests a change affects, then a kit's main or a single .fen file's,
      built through LLVM a function at a time, or with the JIT where LLVM can't build it yet
  fennec run --no-test [path] [args]  run it without running the tests first
  fennec run --jit [path] [args]  run it with the JIT instead of LLVM
  fennec run --interp [path]  run it with the interpreter instead of compiling it
  fennec build [--release] [--cpu baseline] [path] [-o out]  compile it to a native program through LLVM;
                                     --release optimizes the whole program as one;
                                     --cpu baseline builds for x86-64 machines older than AVX2
  fennec test [path]        run a kit's tests, or a single file's, skipping unchanged ones that passed
  fennec test --all [path]  run every test
  fennec test --real [path]  run every test with real capabilities rather than fakes
  fennec test --verbose [path]  list the tests that pass too, not only failures and the summary
  fennec test --jit [path]  run them on the JIT instead of LLVM
  fennec bench [path]       run its `bench` blocks: instructions and times, against the last run
  fennec new <name>         start a kit in a new folder
  fennec init               set up this folder: a kit if it's new, and Fennec's guidance in AGENTS.md
  fennec fmt [path...]      format files in place: a kit, a folder or a .fen file
  fennec fmt --check        list the files that aren't formatted, and fail if any aren't
  fennec check [path]       what the type checker finds, without running anything
  fennec lint [path]        what the checker and linter find, and hints about what may copy or allocate;
                            add --json to either for JSON on standard output
  fennec fix [path]         make the fixes the type checker and linter suggest, in a kit or a .fen file
  fennec lsp                the language server, for editors, over standard input and output
  fennec notices <program>  the third-party notices a program built by `fennec build` holds
  fennec daemon <action>    the build daemon: `status` or `stop`; commands start it
  fennec --version          show the version

  path is a .fen file, or a kit's folder; it defaults to the current folder";

/// The interpreter recurses for each nested call, so it gets a generous stack.
pub(crate) const STACK_BYTES: usize = 512 * 1024 * 1024;

/// The command's arguments, with `--cpu baseline` taken out wherever it's put: it builds for the
/// first processors of the architecture. Nothing when `--cpu` is given something else.
fn arguments() -> Option<Vec<String>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(at) = args.iter().position(|arg| arg == "--cpu") {
        if args.get(at + 1).map(String::as_str) != Some("baseline") {
            eprintln!("error: `--cpu` takes `baseline`, for older machines");
            return None;
        }
        args.drain(at..at + 2);
        aot::BASELINE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // `build --llvm` is what asked for LLVM before every build went through it. It's still taken,
    // as the benchmarks build the commit before this one with the same command.
    if args.first().is_some_and(|arg| arg == "build") && args.get(1).is_some_and(|arg| arg == "--llvm") {
        args.remove(1);
    }
    Some(args)
}

fn main() -> ExitCode {
    fennec_interp::use_tls();
    let Some(args) = arguments() else {
        return ExitCode::FAILURE;
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["run"] => run(run::Engine::Native, ".", &[]),
        ["run", "--no-test"] => run(run::Engine::Untested, ".", &[]),
        ["run", "--no-test", path, arguments @ ..] => run(run::Engine::Untested, path, arguments),
        ["run", "--interp"] => run(run::Engine::Interpreter, ".", &[]),
        ["run", "--interp", path, arguments @ ..] => run(run::Engine::Interpreter, path, arguments),
        ["run", "--jit"] => run(run::Engine::NativeNoted, ".", &[]),
        ["run", "--jit", path, arguments @ ..] => run(run::Engine::NativeNoted, path, arguments),
        ["run", path, arguments @ ..] => run(run::Engine::Native, path, arguments),
        ["build", "--release"] => aot::build(".", None, true),
        ["build", "--release", "-o", output] => aot::build(".", Some(output), true),
        ["build", "--release", path] => aot::build(path, None, true),
        ["build", "--release", path, "-o", output] => aot::build(path, Some(output), true),
        ["build"] => aot::build(".", None, false),
        ["build", "-o", output] => aot::build(".", Some(output), false),
        ["build", path] => aot::build(path, None, false),
        ["build", path, "-o", output] => aot::build(path, Some(output), false),
        ["test", rest @ ..] => test(rest),
        ["bench"] => bench::bench("."),
        ["bench", path] => bench::bench(path),
        ["new", path] => new(path),
        ["init"] => init(),
        ["fmt", rest @ ..] => fmt::fmt(rest),
        ["lint"] => on_big_stack(lint::lint, "."),
        ["lint", "--json"] => on_big_stack(lint::lint_json, "."),
        ["lint", "--json", path] => on_big_stack(lint::lint_json, path),
        ["lint", path] => on_big_stack(lint::lint, path),
        ["check"] => on_big_stack(lint::check, "."),
        ["check", "--json"] => on_big_stack(lint::check_json, "."),
        ["check", "--json", path] => on_big_stack(lint::check_json, path),
        ["check", path] => on_big_stack(lint::check, path),
        ["fix"] => fix::fix("."),
        ["fix", path] => fix::fix(path),
        ["dev", "bench", path, name, rest @ ..] if rest.is_empty() || *rest == ["--start-only"] => {
            let (path, name, execute) = ((*path).to_owned(), (*name).to_owned(), rest.is_empty());
            big_stack(move || bench::one(&path, &name, execute))
        }
        ["dev", "startup", path] => {
            let path = (*path).to_owned();
            big_stack(move || run::startup(&path))
        }
        ["dev", "tokens", path] => dev(fennec_syntax::dump::tokens, path),
        ["dev", "tree", path] => dev(fennec_syntax::dump::tree, path),
        ["dev", "check", path] => dev(check_dump, path),
        ["daemon", action] => daemon::command(action),
        ["lsp"] => lsp::lsp(),
        ["notices", program] => notices::command(program),
        ["dev", "checks"] => on_big_stack(fallbacks::checks, "."),
        ["dev", "checks", path] => on_big_stack(fallbacks::checks, path),
        ["dev", "waits"] => on_big_stack(fallbacks::waits, "."),
        ["dev", "waits", path] => on_big_stack(fallbacks::waits, path),
        ["dev", "fallbacks"] => on_big_stack(fallbacks::fallbacks, "."),
        ["dev", "fallbacks", "--llvm"] => on_big_stack(fallbacks::not_lowered, "."),
        ["dev", "fallbacks", "--llvm", path] => on_big_stack(fallbacks::not_lowered, path),
        ["dev", "fallbacks", path] => on_big_stack(fallbacks::fallbacks, path),
        ["dev", "copies"] => on_big_stack(copies::copies, "."),
        ["dev", "copies", "--run"] => on_big_stack(copies::copies_in_a_run, "."),
        ["dev", "copies", "--run", path] => on_big_stack(copies::copies_in_a_run, path),
        ["dev", "copies", path] => on_big_stack(copies::copies, path),
        ["dev", "trace", rest @ ..] => trace::command(rest),
        ["dev", "reference"] => reference(false),
        ["dev", "reference", "--check"] => reference(true),
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

/// `fennec dev reference [--check]`: writes the language reference, or checks it can be.
fn reference(check: bool) -> ExitCode {
    match fennec_reference::write(check) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn on_big_stack(command: fn(&str) -> ExitCode, path: &str) -> ExitCode {
    let path = path.to_owned();
    big_stack(move || command(&path))
}

/// `fennec test [--all] [--real] [--verbose] [--jit] [path]`, on a big stack for the interpreter.
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

fn init() -> ExitCode {
    let done = match fennec_kit::init(std::path::Path::new(".")) {
        Ok(done) => done,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(name) = &done.kit {
        println!("🦊 made the kit `{name}` here");
        println!("   run it with: fennec run");
    }
    println!(
        "{}",
        match done.guidance {
            fennec_kit::Guidance::Added => "added Fennec's guidance to AGENTS.md",
            fennec_kit::Guidance::Refreshed => "refreshed Fennec's guidance in AGENTS.md",
            fennec_kit::Guidance::AlreadyCurrent => "Fennec's guidance in AGENTS.md is already current",
        }
    );
    ExitCode::SUCCESS
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
