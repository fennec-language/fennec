//! `std.process`: the methods of the `Process` capability. A program is run directly, with its
//! arguments as a list, so nothing in them is ever read by a shell. A program that can't be
//! started fails with a `ProcessError` value; one that runs and exits with a status other
//! than 0 doesn't fail, since what that means is the program's to say. In a test, `Process` is
//! a fake that runs nothing and gives back what the test scripted.

use std::io::Read as _;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;

/// What `run` was given besides the program and its arguments: what the program reads from its
/// standard input, the folder it runs in, and variables it gets on top of this program's own.
#[derive(Default)]
pub(crate) struct Options {
    input: Option<Vec<u8>>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    /// How long it may run before it's stopped, `TIMEOUT` unless given.
    timeout: Option<Duration>,
    /// The most bytes of each of its outputs that are kept, `MOST_OUTPUT` unless given.
    most_output: Option<usize>,
}

/// How long a program may run unless `run` is given `timeout=`: long enough for a build or a
/// clone, short enough that one that hangs doesn't hold what ran it for good.
const TIMEOUT: Duration = Duration::from_secs(600);

/// The most bytes of each of a program's outputs kept unless `run` is given `most_output=`, so
/// one that writes without end can't take the memory with it.
const MOST_OUTPUT: usize = 16 * 1024 * 1024;

/// The words of a `process.run(t"...")`: its text split at spaces, with each value part of the
/// word it's written in, whatever it holds, so a value can't add a word or end one. `None` if
/// this isn't a template of text.
pub(crate) fn words_of(template: &Value) -> Option<Vec<String>> {
    let Value::Instance(instance) = template else { return None };
    let (Value::List(strings), Value::List(values)) = (instance.field("strings")?, instance.field("values")?)
    else {
        return None;
    };
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    for (at, piece) in strings.iter().enumerate() {
        let Value::Str(piece) = piece else { return None };
        for letter in piece.chars() {
            if letter.is_whitespace() {
                words.extend(word.take());
            } else {
                word.get_or_insert_default().push(letter);
            }
        }
        match values.get(at) {
            Some(Value::Str(value)) => word.get_or_insert_default().push_str(value),
            Some(_) => return None,
            None => {}
        }
    }
    words.extend(word);
    Some(words)
}

/// The first value of a `process.run(t"...")` that starts an argument with `-`, by how it's
/// written in its `{}`: the program would read it as an option, like `--upload-pack=` naming a
/// program for git to run, rather than as what it was meant to be. A value later in an argument,
/// like `t"--depth={depth}"`, a `-` in the template's own text, and a value after an argument the
/// template wrote as `--`, which ends the options for most programs, are what the template says.
pub(crate) fn option_started(template: &Value) -> Option<String> {
    let Value::Instance(instance) = template else { return None };
    let (Value::List(strings), Value::List(values)) = (instance.field("strings")?, instance.field("values")?)
    else {
        return None;
    };
    let expressions = instance.field("expressions");
    // Whether the argument being read has anything in it yet, the template's own text of it, and
    // whether an argument the template wrote as `--` has ended the options already.
    let (mut started, mut written, mut options_ended) = (false, String::new(), false);
    for (at, piece) in strings.iter().enumerate() {
        let Value::Str(piece) = piece else { return None };
        for letter in piece.chars() {
            if letter.is_whitespace() {
                options_ended = options_ended || (started && written == "--");
                started = false;
                written.clear();
            } else {
                started = true;
                written.push(letter);
            }
        }
        if let Some(Value::Str(value)) = values.get(at) {
            if !started && !options_ended && value.starts_with('-') {
                let written = match &expressions {
                    Some(Value::List(written)) => match written.get(at) {
                        Some(Value::Str(expression)) => expression.to_string(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                return Some(written);
            }
            if !value.is_empty() {
                started = true;
                // A value in it makes it more than the template's own `--`.
                written.push('\0');
            }
        }
    }
    None
}

/// What a call to `run` asks to run: the program, its arguments and the options.
pub(crate) type Ran = (String, Vec<String>, Options);

/// The program, its arguments and the options of a call to `run`, named as `options_on_name`
/// names it, or the `ProcessError` it fails with, without running anything: a template with a
/// value that would be read as an option.
pub(crate) fn run_of(method: &str, args: &[Value], span: Span) -> Result<Result<Ran, Value>, Diagnostic> {
    let names: Vec<&str> = method.split('.').skip(1).collect();
    let (plain, given) = args.split_at(args.len().saturating_sub(names.len()));
    let (program, arguments) = match plain {
        [Value::Str(program), Value::List(items)] => {
            let mut arguments = Vec::with_capacity(items.len());
            for item in items.iter() {
                let Value::Str(text) = item else { return Err(wrong("run", span)) };
                arguments.push(text.to_string());
            }
            (program.to_string(), arguments)
        }
        [template] => {
            let mut words = words_of(template).ok_or_else(|| wrong("run", span))?.into_iter();
            let program =
                words.next().ok_or_else(|| Diagnostic::new("this template names no program to run", span))?;
            if let Some(expression) = option_started(template) {
                return Ok(Err(looks_like_an_option(&program, &expression)));
            }
            (program, words.collect())
        }
        _ => return Err(wrong("run", span)),
    };
    let mut options = Options::default();
    for (name, value) in names.iter().zip(given) {
        match (*name, value) {
            ("input", Value::Str(text)) => options.input = Some(text.as_bytes().to_vec()),
            ("input", Value::Bytes(data)) => options.input = Some(data.to_vec()),
            ("cwd", folder) => {
                let Value::Str(folder) = crate::stdlib::fs::path_text(folder) else {
                    return Err(wrong("run", span));
                };
                options.cwd = Some(folder.to_string());
            }
            ("env", Value::Dict(pairs)) => {
                for (key, value) in pairs.iter() {
                    let (Value::Str(key), Value::Str(value)) = (key, value) else {
                        return Err(wrong("run", span));
                    };
                    options.env.push((key.to_string(), value.to_string()));
                }
            }
            ("timeout", Value::Instance(duration)) => {
                let Some(Value::Int(nanos)) = duration.field("nanos") else { return Err(wrong("run", span)) };
                options.timeout = Some(Duration::from_nanos(u64::try_from(*nanos).unwrap_or(0)));
            }
            ("most_output", Value::Int(most)) => {
                options.most_output = Some(usize::try_from(*most).unwrap_or(0));
            }
            (other, _) => {
                return Err(Diagnostic::new(format!("`run` has no parameter called `{other}`"), span));
            }
        }
    }
    if let Some(why) = unrunnable(&program, &arguments, &options) {
        return Ok(Err(process_error(&program, &why)));
    }
    Ok(Ok((program, arguments, options)))
}

/// Why a program can't be given what `run` was, or `None` when it can: a NUL byte, which ends
/// the text a program is handed, in its name, an argument or a variable's value, or a variable's
/// name that's empty or holds `=`. The system reads a name with `=` in it as the name before the
/// `=`, so `{"PATH=/evil:": "x"}` would set `PATH`, past a program's check of which names it
/// passes on. A fake `Process` refuses them too, so a test sees what a program would.
fn unrunnable(program: &str, arguments: &[String], options: &Options) -> Option<String> {
    if program.contains('\0') {
        return Some("its name holds a NUL byte, which no program's name can".to_owned());
    }
    if let Some(at) = arguments.iter().position(|argument| argument.contains('\0')) {
        return Some(format!("argument {} holds a NUL byte, which no program can be given", at + 1));
    }
    for (name, value) in &options.env {
        if !crate::env::is_a_name(name) {
            return Some(format!(
                "{name:?} can't be a variable's name: one is never empty, and holds no `=` or NUL byte"
            ));
        }
        if value.contains('\0') {
            return Some(format!("the variable `{name}` holds a NUL byte, which no variable can"));
        }
    }
    None
}

/// The failure of a template whose value `expression` starts an argument with `-`.
fn looks_like_an_option(program: &str, expression: &str) -> Value {
    let fields = vec![("program", Value::str(program)), ("expression", Value::str(expression))];
    Value::Err(Rc::new(Value::made_case("ProcessError", "LooksLikeAnOption", fields)))
}

/// Runs `program` and waits for it, giving a result holding what it wrote. It waits on a thread
/// that may block, so a task running a program doesn't hold its worker. A program still running
/// at its timeout, or that writes more than may be kept, is stopped, and fails as which it was.
pub(crate) fn run_program(program: &str, arguments: &[String], options: &Options) -> Value {
    if let Some(folder) = &options.cwd
        && !std::path::Path::new(folder).is_dir()
    {
        return process_error(program, &format!("there's no folder `{folder}` to run it in"));
    }
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    if let Some(folder) = &options.cwd {
        command.current_dir(folder);
    }
    command.envs(options.env.iter().map(|(name, value)| (name, value)));
    let input = options.input.clone();
    let timeout = options.timeout.unwrap_or(TIMEOUT);
    let most = options.most_output.unwrap_or(MOST_OUTPUT);
    match crate::stdlib::fs::blocking(move || run_limited(command, input, timeout, most)) {
        Ok(Ending::Done(done)) => {
            let status = done.status.code().map_or(-1, i64::from);
            let fields = vec![
                ("status", Value::Int(status)),
                ("stdout", Value::str(&String::from_utf8_lossy(&done.stdout))),
                ("stderr", Value::str(&String::from_utf8_lossy(&done.stderr))),
            ];
            Value::Ok(Rc::new(Value::made("Output", fields)))
        }
        Ok(Ending::TimedOut) => {
            let nanos = i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX);
            let after = Value::made("Duration", vec![("nanos", Value::Int(nanos))]);
            failed(program, "TimedOut", vec![("after", after)])
        }
        Ok(Ending::TooMuchOutput) => failed(
            program,
            "TooMuchOutput",
            vec![("most", Value::Int(i64::try_from(most).unwrap_or(i64::MAX)))],
        ),
        Err(error) => process_error(program, &error.to_string()),
    }
}

/// How a program run within its limits ended.
enum Ending {
    Done(std::process::Output),
    TimedOut,
    TooMuchOutput,
}

/// A failed result holding the `ProcessError` case `case` for `program`, with `fields` after it.
fn failed(program: &str, case: &str, fields: Vec<(&str, Value)>) -> Value {
    let mut all = vec![("program", Value::str(program))];
    all.extend(fields);
    Value::Err(Rc::new(Value::made_case("ProcessError", case, all)))
}

/// How long the readers of a program's outputs are given once it's been stopped, before what
/// they've read is taken as all there is: a program it started that got out of its process group
/// can hold the pipes open for as long as it likes.
const GRACE: Duration = Duration::from_millis(500);

/// Runs `command`, writing `input` to its standard input on a thread of its own and reading each
/// of its outputs on another, so none waits on the rest. It's stopped once `timeout` has passed,
/// or one of its outputs has more than `most` bytes. A program that stops reading early leaves
/// the rest of its input unwritten, which isn't an error.
///
/// On Unix it runs in a process group of its own, and stopping it kills the group, so what it
/// started, as `sh -c` and `make` do, is stopped with it rather than holding the pipes and running
/// on. The timeout covers the outputs too: a program that ends while something it started still
/// holds them hasn't finished until they close.
fn run_limited(
    mut command: std::process::Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    most: usize,
) -> std::io::Result<Ending> {
    use std::io::Write as _;
    use std::process::Stdio;
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    command.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn()?;
    // Never waited for: a program that stops reading leaves it to fail on a closed pipe.
    if let Some((mut stdin, input)) = child.stdin.take().zip(input) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let over = Arc::new(AtomicBool::new(false));
    let (finished, read_all) = std::sync::mpsc::channel::<()>();
    let stdout = read_kept(child.stdout.take().map(|pipe| Box::new(pipe) as Pipe), most, &over, &finished);
    let stderr = read_kept(child.stderr.take().map(|pipe| Box::new(pipe) as Pipe), most, &over, &finished);
    drop(finished);
    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut readers_done = 0;
    let stopped = loop {
        if status.is_none() {
            status = child.try_wait()?;
        }
        while read_all.try_recv().is_ok() {
            readers_done += 1;
        }
        if over.load(Ordering::SeqCst) || Instant::now() >= deadline {
            break true;
        }
        if status.is_some() && readers_done == 2 {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if stopped {
        stop(&mut child);
        let given = Instant::now() + GRACE;
        while readers_done < 2 {
            let left = given.saturating_duration_since(Instant::now());
            if left.is_zero() || read_all.recv_timeout(left).is_err() {
                break;
            }
            readers_done += 1;
        }
    }
    let taken = |kept: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *kept.lock().unwrap_or_else(PoisonError::into_inner))
    };
    let (stdout, stderr) = (taken(&stdout), taken(&stderr));
    if over.load(Ordering::SeqCst) {
        return Ok(Ending::TooMuchOutput);
    }
    Ok(match status {
        Some(status) if !stopped => Ending::Done(std::process::Output { status, stdout, stderr }),
        _ => Ending::TimedOut,
    })
}

/// One of a program's outputs.
type Pipe = Box<dyn std::io::Read + Send>;

/// Reads `pipe` on a thread of its own, keeping at most one byte past `most`, and setting `over`
/// once it's past. What it's read so far is kept where it can be taken without waiting for it to
/// end, and `finished` hears when it has.
fn read_kept(
    pipe: Option<Pipe>,
    most: usize,
    over: &Arc<AtomicBool>,
    finished: &std::sync::mpsc::Sender<()>,
) -> Arc<Mutex<Vec<u8>>> {
    let kept = Arc::new(Mutex::new(Vec::new()));
    let (held, over, finished) = (Arc::clone(&kept), Arc::clone(over), finished.clone());
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = vec![0; 64 * 1024];
            loop {
                let read = match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let mut kept = held.lock().unwrap_or_else(PoisonError::into_inner);
                // One byte past the most, to tell it went past.
                let room = (most + 1).saturating_sub(kept.len());
                kept.extend_from_slice(chunk.get(..read.min(room)).unwrap_or_default());
                if kept.len() > most {
                    over.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
        let _ = finished.send(());
    });
    kept
}

/// Stops a program and, on Unix, everything in its process group, which is what it started.
fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let group = rustix::process::Pid::from_child(child);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A failed result holding a `ProcessError`.
pub(crate) fn process_error(program: &str, message: &str) -> Value {
    let fields = vec![("program", Value::str(program)), ("reason", Value::str(message))];
    Value::Err(Rc::new(Value::made_case("ProcessError", "NotRun", fields)))
}

fn wrong(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Options, run_program, unrunnable};
    use crate::value::Value;

    /// What a run gave: the `Output`'s fields, or the `ProcessError`'s case and its fields.
    fn said(ran: &Value) -> String {
        crate::format::repr(ran)
    }

    /// A program that never ends is stopped at its timeout, rather than holding what ran it.
    #[test]
    fn a_child_that_never_ends_is_stopped_at_its_timeout() {
        let options = Options { timeout: Some(Duration::from_millis(200)), ..Options::default() };
        let started = Instant::now();
        let ran = run_program("sleep", &["30".to_owned()], &options);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        let shown = said(&ran);
        assert!(shown.contains("ProcessError.TimedOut") && shown.contains("program=\"sleep\""), "{shown}");
    }

    /// A program that starts another, as `sh -c` does, is stopped with what it started at its
    /// timeout: what it started held the pipes, so the run went on until that ended, and it ran
    /// on afterwards.
    #[test]
    fn what_a_program_started_is_stopped_with_it() {
        let folder = std::env::temp_dir().join(format!("fennec-process-group-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let noted = folder.join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", noted.display());
        let options = Options { timeout: Some(Duration::from_millis(500)), ..Options::default() };
        let started = Instant::now();
        let ran = run_program("sh", &["-c".to_owned(), script], &options);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(said(&ran).contains("ProcessError.TimedOut"), "{}", said(&ran));
        // The `sleep` it started is gone too, once the system has taken the kill.
        let pid = std::fs::read_to_string(&noted).unwrap().trim().to_owned();
        let gone = (0..200).any(|_| {
            let alive = std::process::Command::new("kill").args(["-0", &pid]).status().unwrap().success();
            if alive {
                std::thread::sleep(Duration::from_millis(10));
            }
            !alive
        });
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(gone, "the `sleep` the program started outlived it");
    }

    /// A program that ends while something it started still holds its outputs hasn't finished:
    /// the timeout covers the outputs closing too.
    #[test]
    fn a_program_whose_outputs_stay_open_after_it_ends_is_stopped_at_its_timeout() {
        let options = Options { timeout: Some(Duration::from_millis(500)), ..Options::default() };
        let started = Instant::now();
        let ran = run_program("sh", &["-c".to_owned(), "sleep 30 &".to_owned()], &options);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(said(&ran).contains("ProcessError.TimedOut"), "{}", said(&ran));
    }

    /// A program that writes without end is stopped once it's written more than may be kept,
    /// rather than taking the memory with it.
    #[test]
    fn a_child_that_writes_without_end_is_stopped_past_the_most_output() {
        let options = Options { most_output: Some(1000), ..Options::default() };
        let ran = run_program("yes", &[], &options);
        let shown = said(&ran);
        assert!(shown.contains("ProcessError.TooMuchOutput") && shown.contains("most=1000"), "{shown}");
    }

    /// A variable's name with `=` in it would set the variable named before the `=`, and a NUL
    /// byte would cut what a program's given short, so neither runs.
    #[test]
    fn a_name_or_text_the_system_would_read_as_other_never_runs() {
        let with = |name: &str, value: &str| Options {
            env: vec![(name.to_owned(), value.to_owned())],
            ..Options::default()
        };
        assert!(unrunnable("env", &[], &with("LANG", "C")).is_none());
        for name in ["PATH=/evil:", "", "A\0B"] {
            let why = unrunnable("env", &[], &with(name, "x")).unwrap_or_default();
            assert!(why.contains("can't be a variable's name"), "{name:?}: {why}");
        }
        let why = unrunnable("env", &[], &with("LANG", "C\0x")).unwrap_or_default();
        assert!(why.contains("the variable `LANG` holds a NUL byte"), "{why}");
        let why = unrunnable("echo", &["a".to_owned(), "b\0c".to_owned()], &Options::default());
        assert_eq!(why.as_deref(), Some("argument 2 holds a NUL byte, which no program can be given"));
        assert!(unrunnable("ec\0ho", &[], &Options::default()).is_some());
    }

    /// Within its limits, a program runs as ever, its output whole.
    #[test]
    fn a_child_within_its_limits_gives_what_it_wrote() {
        let options = Options {
            timeout: Some(Duration::from_secs(30)),
            most_output: Some(5),
            input: Some(b"typed".to_vec()),
            ..Options::default()
        };
        let shown = said(&run_program("cat", &[], &options));
        assert!(shown.contains("status=0") && shown.contains("stdout=\"typed\""), "{shown}");
    }
}
